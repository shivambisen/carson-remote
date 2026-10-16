package signaling

import (
	"context"
	"encoding/json"
	"errors"
	"log/slog"
	"net/http"
	"time"

	"github.com/coder/websocket"

	"carson/server/internal/audit"
	"carson/server/internal/auth"
	"carson/server/internal/devices"
	"carson/server/internal/httpx"
	"carson/server/internal/protocol"
	"carson/server/internal/ratelimit"
	"carson/server/internal/store"
	"carson/server/internal/turn"
)

// Close codes in the 4000-4999 private range.
const (
	CloseReplaced websocket.StatusCode = 4000 // same device connected again
	CloseTimeout  websocket.StatusCode = 4001 // heartbeat missed
	CloseSlow     websocket.StatusCode = 4002 // client could not keep up
	CloseRevoked  websocket.StatusCode = 4003 // device revoked by its owner
)

// Handler serves /v1/ws for both viewers (session cookie) and agents (bearer token).
type Handler struct {
	Hub     *Hub
	Store   *store.Store
	Auth    *auth.Service
	Devices *devices.Service
	Log     *slog.Logger
	Now     func() time.Time
	// PingInterval and PingTimeout bound how long a dead agent stays "online":
	// at most PingInterval + PingTimeout (15 s + 30 s = 45 s by default).
	PingInterval time.Duration
	PingTimeout  time.Duration
	// ICE issues STUN/TURN servers (with time-limited TURN credentials) per session.
	ICE *turn.Issuer
	// ResumeWindow is how long a session waits for a dropped party (default 30 s).
	ResumeWindow time.Duration
	// SessionLimiter bounds session.request per user.
	SessionLimiter *ratelimit.Limiter
	TrustProxy     bool
	Audit          *audit.Log
	// UpgradeLimiter bounds WebSocket connects per IP; ResumeLimiter bounds session.resume
	// attempts per account/device. Both nil-safe.
	UpgradeLimiter *ratelimit.Limiter
	ResumeLimiter  *ratelimit.Limiter
	// MessageRate / MessageBurst bound inbound messages per connection (0 = defaults).
	MessageRate  int
	MessageBurst int
}

func (h *Handler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if h.UpgradeLimiter != nil && !h.UpgradeLimiter.Allow(httpx.ClientIP(r, h.TrustProxy)) {
		httpx.WriteError(w, http.StatusTooManyRequests, "rate_limited", "too many connections")
		return
	}
	if r.Header.Get("Authorization") != "" {
		h.serveAgent(w, r)
		return
	}
	h.serveViewer(w, r)
}

func (h *Handler) accept(w http.ResponseWriter, r *http.Request) (*websocket.Conn, error) {
	// Origin is verified by the caller (viewers) or irrelevant (agents carry no cookies),
	// so the library's same-host check is disabled in favour of our allowlist.
	conn, err := websocket.Accept(w, r, &websocket.AcceptOptions{InsecureSkipVerify: true})
	if err != nil {
		return nil, err
	}
	conn.SetReadLimit(protocol.MaxMessageBytes)
	return conn, nil
}

func (h *Handler) serveViewer(w http.ResponseWriter, r *http.Request) {
	if !h.Auth.OriginAllowed(r) {
		httpx.WriteError(w, http.StatusForbidden, "bad_origin", "request origin not allowed")
		return
	}
	u, err := h.Auth.SessionUser(r)
	if err != nil {
		httpx.WriteError(w, http.StatusUnauthorized, "unauthenticated", "sign in required")
		return
	}
	list, err := h.Store.ListDevices(r.Context(), u.ID)
	if err != nil {
		h.Log.Error("list devices", "err", err)
		httpx.WriteError(w, http.StatusInternalServerError, "internal", "internal error")
		return
	}
	conn, err := h.accept(w, r)
	if err != nil {
		return
	}
	ctx, cancel := context.WithCancel(r.Context())
	defer cancel()
	c := newClient(u.ID, "", closer(conn, cancel))

	// Register before snapshotting presence so no transition is missed.
	h.Hub.addViewer(c)
	defer h.Hub.removeViewer(c)
	views := make([]devices.DeviceView, len(list))
	for i, d := range list {
		views[i] = devices.View(d, h.Hub.Online(d.ID))
	}
	hello, _ := protocol.NewEnvelope(protocol.TypeHelloOK, map[string]any{"user": u, "devices": views})
	c.enqueue(hello)

	ua := r.UserAgent()
	if len(ua) > 200 {
		ua = ua[:200]
	}
	viewer := protocol.ViewerInfo{Email: u.Email, IP: httpx.ClientIP(r, h.TrustProxy), UserAgent: ua}
	defer h.detachAll(c)

	go h.writeLoop(ctx, conn, c)
	// Viewers get pings too: reverse proxies (Cloudflare, many load balancers) close WebSockets
	// that are idle for ~100 s, which would drop the viewer between sessions.
	go h.heartbeat(ctx, conn, c)
	h.readLoop(ctx, conn, c, func(env protocol.Envelope) {
		h.handleViewerSession(c, viewer, env)
	})
}

func (h *Handler) serveAgent(w http.ResponseWriter, r *http.Request) {
	d, err := h.Devices.DeviceFromRequest(r.Context(), r)
	if errors.Is(err, store.ErrNotFound) {
		httpx.WriteError(w, http.StatusUnauthorized, "invalid_token", "device token invalid or expired")
		return
	}
	if err != nil {
		h.Log.Error("device auth", "err", err)
		httpx.WriteError(w, http.StatusInternalServerError, "internal", "internal error")
		return
	}
	conn, err := h.accept(w, r)
	if err != nil {
		return
	}
	ctx, cancel := context.WithCancel(r.Context())
	defer cancel()
	c := newClient(d.UserID, d.ID, closer(conn, cancel))
	log := h.Log.With("device_id", d.ID, "user_id", d.UserID)

	if old := h.Hub.addAgent(c); old != nil {
		old.close("replaced")
	}
	h.touch(d.ID, "", "")
	h.announce(d.UserID, d.ID, true)
	log.Info("agent online")
	defer h.detachAll(c)
	defer func() {
		if h.Hub.removeAgent(c) {
			h.touch(d.ID, "", "")
			h.announce(d.UserID, d.ID, false)
			log.Info("agent offline")
		}
	}()

	go h.writeLoop(ctx, conn, c)
	go h.heartbeat(ctx, conn, c)
	h.readLoop(ctx, conn, c, func(env protocol.Envelope) {
		switch env.Type {
		case protocol.TypeHello:
			var hello protocol.AgentHello
			if err := json.Unmarshal(env.Body, &hello); err != nil {
				c.close("bad hello")
				return
			}
			if hello.OS != "" && !devices.AllowedOS[hello.OS] {
				hello.OS = ""
			}
			if len(hello.AgentVersion) > 32 {
				hello.AgentVersion = hello.AgentVersion[:32]
			}
			h.touch(d.ID, hello.AgentVersion, hello.OS)
			ok, _ := protocol.NewEnvelope(protocol.TypeHelloOK, protocol.AgentHelloOK{DeviceID: d.ID, HeartbeatInterval: int(h.PingInterval.Seconds())})
			c.enqueue(ok)
		default:
			h.handleAgentSession(c, env)
		}
	})
}

// closer returns the close function for a client: close the socket with a reason and stop its loops.
func closer(conn *websocket.Conn, cancel context.CancelFunc) func(string) {
	return func(reason string) {
		code := websocket.StatusNormalClosure
		switch reason {
		case "replaced":
			code = CloseReplaced
		case "heartbeat timeout":
			code = CloseTimeout
		case "slow consumer":
			code = CloseSlow
		case "revoked":
			code = CloseRevoked
		case "bad hello", "bad message", "rate limited":
			code = websocket.StatusPolicyViolation
		}
		if code == CloseTimeout {
			conn.CloseNow() // peer is unresponsive; skip the close handshake
			cancel()
			return
		}
		// Close performs the handshake, which needs the read loop still running,
		// so run it async and only then cancel the loops.
		go func() {
			conn.Close(code, reason)
			cancel()
		}()
	}
}

func (h *Handler) readLoop(ctx context.Context, conn *websocket.Conn, c *client, handle func(protocol.Envelope)) {
	defer c.close("read done")
	rate, burst := h.MessageRate, h.MessageBurst
	if rate <= 0 {
		rate, burst = 200, 400 // trickle ICE bursts are ~tens of messages
	}
	flood := ratelimit.New(rate, time.Second, burst) // per connection
	for {
		typ, data, err := conn.Read(ctx)
		if err != nil {
			return
		}
		if !flood.Allow("") {
			h.Log.Warn("closing flooding connection", "user_id", c.userID, "device_id", c.deviceID)
			c.close("rate limited")
			return
		}
		var env protocol.Envelope
		if typ != websocket.MessageText || json.Unmarshal(data, &env) != nil || env.V != protocol.Version {
			c.close("bad message")
			return
		}
		handle(env)
	}
}

func (h *Handler) writeLoop(ctx context.Context, conn *websocket.Conn, c *client) {
	for {
		select {
		case <-ctx.Done():
			return
		case msg := <-c.send:
			wctx, cancel := context.WithTimeout(ctx, 10*time.Second)
			err := conn.Write(wctx, websocket.MessageText, msg)
			cancel()
			if err != nil {
				c.close("write failed")
				return
			}
		}
	}
}

// heartbeat pings the client; a missed pong closes the connection (for an agent, that marks it
// offline). For agents, successful pings refresh last_seen.
func (h *Handler) heartbeat(ctx context.Context, conn *websocket.Conn, c *client) {
	t := time.NewTicker(h.PingInterval)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
			pctx, cancel := context.WithTimeout(ctx, h.PingTimeout)
			err := conn.Ping(pctx)
			cancel()
			if err != nil {
				c.close("heartbeat timeout")
				return
			}
			if c.deviceID != "" {
				h.touch(c.deviceID, "", "")
			}
		}
	}
}

func (h *Handler) touch(deviceID, version, os string) {
	// Detached context: this also runs during teardown after the request context is gone.
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := h.Store.TouchDevice(ctx, deviceID, h.Now(), version, os); err != nil {
		h.Log.Error("touch device", "device_id", deviceID, "err", err)
	}
}

func (h *Handler) announce(userID, deviceID string, online bool) {
	msg, _ := protocol.NewEnvelope(protocol.TypePresence, protocol.PresenceBody{DeviceID: deviceID, Online: online, LastSeenAt: h.Now().UTC()})
	h.Hub.toViewers(userID, msg)
}
