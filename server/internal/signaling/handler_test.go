package signaling

import (
	"context"
	"crypto/ed25519"
	"encoding/json"
	"errors"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
	"time"

	"github.com/coder/websocket"

	"carson/server/internal/audit"
	"carson/server/internal/auth"
	"carson/server/internal/devices"
	"carson/server/internal/protocol"
	"carson/server/internal/ratelimit"
	"carson/server/internal/store"
	"carson/server/internal/store/storetest"
	"carson/server/internal/turn"
)

type env struct {
	srv     *httptest.Server
	store   *store.Store
	hub     *Hub
	handler *Handler
}

func newEnv(t *testing.T) *env {
	t.Helper()
	return newEnvWithHeartbeat(t, 50*time.Millisecond, 100*time.Millisecond)
}

func newEnvWithHeartbeat(t *testing.T, interval, timeout time.Duration) *env {
	t.Helper()
	st := storetest.Open(t)
	mux := http.NewServeMux()
	srv := httptest.NewServer(mux)
	t.Cleanup(srv.Close)
	pub, _ := url.Parse(srv.URL)
	log := slog.New(slog.NewTextHandler(io.Discard, nil))
	lim := ratelimit.New(1000, time.Minute, 1000)
	a := &auth.Service{Store: st, PublicURL: pub, Log: log, Now: time.Now, LoginLimiter: lim}
	hub := NewHub()
	d := &devices.Service{Store: st, Auth: a, Presence: hub, Log: log, Now: time.Now}
	h := &Handler{
		Hub: hub, Store: st, Auth: a, Devices: d, Log: log, Now: time.Now,
		PingInterval: interval, PingTimeout: timeout,
		// Test viewers only read when a test expects a message; keep their pings out of the way.
		ViewerPingInterval: time.Hour,
		ResumeWindow:       300 * time.Millisecond,
		ICE:                &turn.Issuer{Secret: "test-secret", TURNURLs: []string{"turn:turn.test:3478?transport=udp"}, STUNURLs: []string{"stun:turn.test:3478"}},
		Audit:              &audit.Log{Store: st, Log: log, Now: time.Now},
		ResumeLimiter:      ratelimit.New(3, time.Minute, 3),
		MessageRate:        50,
		MessageBurst:       50,
	}
	mux.Handle("GET /v1/ws", h)
	return &env{srv: srv, store: st, hub: hub, handler: h}
}

func (e *env) wsURL() string { return "ws" + strings.TrimPrefix(e.srv.URL, "http") + "/v1/ws" }

// session creates a user with a web session and returns its cookie value.
func (e *env) session(t *testing.T, email string) (store.User, string) {
	t.Helper()
	ctx := context.Background()
	u, err := e.store.UpsertUserForIdentity(ctx, store.Identity{Provider: "dev", Subject: email, Email: email}, time.Now())
	if err != nil {
		t.Fatal(err)
	}
	tok, hash := store.NewToken()
	if err := e.store.CreateWebSession(ctx, hash, u.ID, time.Now(), time.Now().Add(time.Hour), "", ""); err != nil {
		t.Fatal(err)
	}
	return u, tok
}

// device enrolls a device for u and returns its ID and a valid access token.
func (e *env) device(t *testing.T, u store.User) (string, string) {
	t.Helper()
	ctx := context.Background()
	pub, _, _ := ed25519.GenerateKey(nil)
	code := store.HashToken(store.NewID("code"))
	if err := e.store.CreatePairing(ctx, store.Pairing{ID: store.NewID("pair"), CodeHash: code, PublicKey: pub, Name: "Mac", OS: "macos", CreatedAt: time.Now(), ExpiresAt: time.Now().Add(time.Minute)}); err != nil {
		t.Fatal(err)
	}
	d, err := e.store.ApprovePairing(ctx, code, u.ID, time.Now())
	if err != nil {
		t.Fatal(err)
	}
	tok, hash := store.NewToken()
	_ = e.store.CreateDeviceToken(ctx, hash, d.ID, time.Now().Add(time.Hour))
	return d.ID, tok
}

func (e *env) dialViewer(t *testing.T, cookie, origin string) (*websocket.Conn, *http.Response, error) {
	t.Helper()
	h := http.Header{}
	h.Set("Cookie", auth.CookieName+"="+cookie)
	if origin != "" {
		h.Set("Origin", origin)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	c, resp, err := websocket.Dial(ctx, e.wsURL(), &websocket.DialOptions{HTTPHeader: h})
	if c != nil {
		t.Cleanup(func() { c.CloseNow() })
	}
	return c, resp, err
}

func (e *env) dialAgent(t *testing.T, token string) (*websocket.Conn, *http.Response, error) {
	t.Helper()
	h := http.Header{}
	h.Set("Authorization", "Bearer "+token)
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	c, resp, err := websocket.Dial(ctx, e.wsURL(), &websocket.DialOptions{HTTPHeader: h})
	if c != nil {
		t.Cleanup(func() { c.CloseNow() })
	}
	return c, resp, err
}

func read(t *testing.T, c *websocket.Conn) protocol.Envelope {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	_, data, err := c.Read(ctx)
	if err != nil {
		t.Fatalf("read: %v", err)
	}
	var env protocol.Envelope
	if err := json.Unmarshal(data, &env); err != nil {
		t.Fatal(err)
	}
	return env
}

func readPresence(t *testing.T, c *websocket.Conn) protocol.PresenceBody {
	t.Helper()
	env := read(t, c)
	if env.Type != protocol.TypePresence {
		t.Fatalf("got %s, want presence", env.Type)
	}
	var p protocol.PresenceBody
	_ = json.Unmarshal(env.Body, &p)
	return p
}

func send(t *testing.T, c *websocket.Conn, typ protocol.MessageType, body any) {
	t.Helper()
	msg, _ := protocol.NewEnvelope(typ, body)
	if err := c.Write(context.Background(), websocket.MessageText, msg); err != nil {
		t.Fatal(err)
	}
}

// keepReading drains an agent connection in the background so pings get answered.
func keepReading(c *websocket.Conn) {
	go func() {
		for {
			if _, _, err := c.Read(context.Background()); err != nil {
				return
			}
		}
	}()
}

func TestViewerSeesPresenceTransitions(t *testing.T) {
	e := newEnv(t)
	u, cookie := e.session(t, "ana@x.com")
	devID, devTok := e.device(t, u)

	viewer, _, err := e.dialViewer(t, cookie, e.srv.URL)
	if err != nil {
		t.Fatal(err)
	}
	hello := read(t, viewer)
	var hb struct {
		User    store.User           `json:"user"`
		Devices []devices.DeviceView `json:"devices"`
	}
	_ = json.Unmarshal(hello.Body, &hb)
	if hello.Type != protocol.TypeHelloOK || hb.User.ID != u.ID || len(hb.Devices) != 1 || hb.Devices[0].Online {
		t.Fatalf("hello.ok = %s %+v", hello.Type, hb)
	}

	agent, _, err := e.dialAgent(t, devTok)
	if err != nil {
		t.Fatal(err)
	}
	if p := readPresence(t, viewer); p.DeviceID != devID || !p.Online {
		t.Fatalf("online presence = %+v", p)
	}
	send(t, agent, protocol.TypeHello, protocol.AgentHello{AgentVersion: "0.1.0", OS: "macos"})
	if env := read(t, agent); env.Type != protocol.TypeHelloOK {
		t.Fatalf("agent got %s", env.Type)
	}
	keepReading(agent)
	if !e.hub.Online(devID) {
		t.Fatal("hub should report device online")
	}

	agent.Close(websocket.StatusNormalClosure, "bye")
	if p := readPresence(t, viewer); p.DeviceID != devID || p.Online {
		t.Fatalf("offline presence = %+v", p)
	}
	d, _ := e.store.GetDevice(context.Background(), devID)
	if d.AgentVersion != "0.1.0" || d.LastSeenAt.IsZero() {
		t.Fatalf("device not touched: %+v", d)
	}
}

func TestPresenceNeverCrossesAccounts(t *testing.T) {
	e := newEnv(t)
	ana, _ := e.session(t, "ana@x.com")
	_, boCookie := e.session(t, "bo@x.com")
	_, anaTok := e.device(t, ana)

	bo, _, err := e.dialViewer(t, boCookie, e.srv.URL)
	if err != nil {
		t.Fatal(err)
	}
	var hb struct{ Devices []devices.DeviceView }
	_ = json.Unmarshal(read(t, bo).Body, &hb)
	if len(hb.Devices) != 0 {
		t.Fatalf("bo sees ana's devices: %+v", hb.Devices)
	}

	agent, _, err := e.dialAgent(t, anaTok)
	if err != nil {
		t.Fatal(err)
	}
	keepReading(agent)
	ctx, cancel := context.WithTimeout(context.Background(), 300*time.Millisecond)
	defer cancel()
	if _, data, err := bo.Read(ctx); err == nil {
		t.Fatalf("bo received another account's event: %s", data)
	}
}

func TestMissedHeartbeatMarksOffline(t *testing.T) {
	e := newEnv(t)
	u, cookie := e.session(t, "ana@x.com")
	devID, tok := e.device(t, u)
	viewer, _, _ := e.dialViewer(t, cookie, e.srv.URL)
	read(t, viewer) // hello.ok

	// This agent never reads, so it never answers pings.
	if _, _, err := e.dialAgent(t, tok); err != nil {
		t.Fatal(err)
	}
	if p := readPresence(t, viewer); !p.Online {
		t.Fatal("expected online")
	}
	start := time.Now()
	if p := readPresence(t, viewer); p.DeviceID != devID || p.Online {
		t.Fatalf("expected offline, got %+v", p)
	}
	if time.Since(start) > time.Second {
		t.Fatalf("offline detection took %s", time.Since(start))
	}
}

// Viewers are pinged too, so proxies that close idle WebSockets (Cloudflare: ~100 s) don't
// drop them, and a viewer that stops answering is cleaned up.
func TestViewerIsPingedAndDeadViewerIsClosed(t *testing.T) {
	e := newEnv(t)
	e.handler.ViewerPingInterval = 50 * time.Millisecond
	_, cookie := e.session(t, "ana@x.com")
	live, _, err := e.dialViewer(t, cookie, e.srv.URL)
	if err != nil {
		t.Fatal(err)
	}
	read(t, live)                                                    // hello.ok
	keepReading(live)                                                // reading answers pings
	if _, _, err := e.dialViewer(t, cookie, e.srv.URL); err != nil { // never reads
		t.Fatal(err)
	}
	deadline := time.Now().Add(2 * time.Second)
	for e.hub.viewerCount() != 1 {
		if time.Now().After(deadline) {
			t.Fatalf("viewers = %d, want only the live one left", e.hub.viewerCount())
		}
		time.Sleep(20 * time.Millisecond)
	}
	time.Sleep(300 * time.Millisecond) // several ping intervals
	if n := e.hub.viewerCount(); n != 1 {
		t.Fatalf("live viewer was dropped (viewers = %d)", n)
	}
}

func TestReconnectReplacesOldAgentWithoutFlapping(t *testing.T) {
	e := newEnv(t)
	u, cookie := e.session(t, "ana@x.com")
	devID, tok := e.device(t, u)
	viewer, _, _ := e.dialViewer(t, cookie, e.srv.URL)
	read(t, viewer)

	old, _, _ := e.dialAgent(t, tok)
	readPresence(t, viewer)
	newer, _, _ := e.dialAgent(t, tok)
	keepReading(newer)
	readPresence(t, viewer) // online again (new connection)

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	_, _, err := old.Read(ctx)
	if websocket.CloseStatus(err) != CloseReplaced {
		t.Fatalf("old agent close = %v, want %d", err, CloseReplaced)
	}
	if !e.hub.Online(devID) {
		t.Fatal("device must stay online after the replaced connection closes")
	}
	ctx2, cancel2 := context.WithTimeout(context.Background(), 200*time.Millisecond)
	defer cancel2()
	if _, data, err := viewer.Read(ctx2); err == nil {
		t.Fatalf("unexpected event after replacement: %s", data)
	}
}

func TestWebSocketAuth(t *testing.T) {
	e := newEnv(t)
	_, cookie := e.session(t, "ana@x.com")

	status := func(_ *websocket.Conn, resp *http.Response, err error) int {
		if err == nil {
			return http.StatusSwitchingProtocols
		}
		if resp == nil {
			t.Fatalf("dial failed without response: %v", err)
		}
		return resp.StatusCode
	}
	if got := status(e.dialViewer(t, cookie, "")); got != http.StatusForbidden {
		t.Errorf("viewer without origin = %d", got)
	}
	if got := status(e.dialViewer(t, cookie, "https://evil.example")); got != http.StatusForbidden {
		t.Errorf("viewer from foreign origin = %d", got)
	}
	if got := status(e.dialViewer(t, "forged", e.srv.URL)); got != http.StatusUnauthorized {
		t.Errorf("viewer with bad cookie = %d", got)
	}
	if got := status(e.dialAgent(t, "forged")); got != http.StatusUnauthorized {
		t.Errorf("agent with bad token = %d", got)
	}
}

func TestMalformedMessageClosesConnection(t *testing.T) {
	e := newEnv(t)
	_, cookie := e.session(t, "ana@x.com")
	viewer, _, _ := e.dialViewer(t, cookie, e.srv.URL)
	read(t, viewer)
	_ = viewer.Write(context.Background(), websocket.MessageText, []byte(`{"v":99,"type":"hello"}`))
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	_, _, err := viewer.Read(ctx)
	if websocket.CloseStatus(err) != websocket.StatusPolicyViolation {
		t.Fatalf("close = %v", err)
	}
	var ce websocket.CloseError
	if !errors.As(err, &ce) {
		t.Fatalf("expected close error, got %v", err)
	}
}

// viewerCount returns the number of connected viewers across all users.
func (h *Hub) viewerCount() int {
	h.mu.Lock()
	defer h.mu.Unlock()
	n := 0
	for _, set := range h.viewers {
		n += len(set)
	}
	return n
}
