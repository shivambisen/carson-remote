package signaling

import (
	"context"
	"crypto/subtle"
	"encoding/json"
	"errors"
	"time"

	"carson/server/internal/protocol"
	"carson/server/internal/store"
)

// Limits on concurrent sessions. One viewer per device until multi-viewer support lands.
const maxSessionsPerViewer = 4

// DefaultResumeWindow is how long a session survives one party's signaling drop.
const DefaultResumeWindow = 30 * time.Second

var (
	errBusy            = errors.New("device busy")
	errTooManySessions = errors.New("too many sessions")
)

// session pairs one viewer with one agent. The server only relays opaque SDP/ICE between
// them; media never touches it. If a party's signaling connection drops, its side is
// detached (nil) for up to the resume window; media keeps flowing peer-to-peer meanwhile.
type session struct {
	id       string
	userID   string
	deviceID string
	viewer   *client // nil while detached
	agent    *client // nil while detached
	accepted bool

	viewerToken []byte // hashes of the parties' resume tokens
	agentToken  []byte
	expiry      *time.Timer // running while a side is detached

	// For the audit log.
	requestedAt time.Time
	acceptedAt  time.Time
	viewerInfo  protocol.ViewerInfo
}

// createSession registers a session between viewer and the device's current agent.
func (h *Hub) createSession(viewer *client, deviceID string, info protocol.ViewerInfo, now time.Time) (*session, string, string, error) {
	h.mu.Lock()
	defer h.mu.Unlock()
	agent := h.agents[deviceID]
	if agent == nil {
		return nil, "", "", store.ErrNotFound
	}
	if len(viewer.sessions) >= maxSessionsPerViewer {
		return nil, "", "", errTooManySessions
	}
	for _, s := range h.sessions { // includes sessions whose agent side is detached
		if s.deviceID == deviceID {
			return nil, "", "", errBusy
		}
	}
	vTok, vHash := store.NewToken()
	aTok, aHash := store.NewToken()
	s := &session{
		id: store.NewID("ses"), userID: viewer.userID, deviceID: deviceID,
		viewer: viewer, agent: agent, viewerToken: vHash, agentToken: aHash,
		requestedAt: now, viewerInfo: info,
	}
	h.sessions[s.id] = s
	viewer.sessions[s.id] = s
	agent.sessions[s.id] = s
	return s, vTok, aTok, nil
}

// sessionFor returns the session sid only if c is currently one of its two parties.
func (h *Hub) sessionFor(sid string, c *client) *session {
	h.mu.Lock()
	defer h.mu.Unlock()
	s := h.sessions[sid]
	if s == nil || (s.viewer != c && s.agent != c) {
		return nil
	}
	return s
}

// peers returns the current viewer and agent (either may be nil while detached).
func (h *Hub) peers(s *session) (viewer, agent *client) {
	h.mu.Lock()
	defer h.mu.Unlock()
	return s.viewer, s.agent
}

func (h *Hub) markAccepted(s *session, now time.Time) {
	h.mu.Lock()
	defer h.mu.Unlock()
	s.accepted = true
	s.acceptedAt = now
}

// deviceSessions snapshots all sessions (attached or detached) for a device.
func (h *Hub) deviceSessions(deviceID string) []*session {
	h.mu.Lock()
	defer h.mu.Unlock()
	var out []*session
	for _, s := range h.sessions {
		if s.deviceID == deviceID {
			out = append(out, s)
		}
	}
	return out
}

// agentFor returns the device's live agent connection, if any.
func (h *Hub) agentFor(deviceID string) *client {
	h.mu.Lock()
	defer h.mu.Unlock()
	return h.agents[deviceID]
}

func (h *Hub) isAccepted(s *session) bool {
	h.mu.Lock()
	defer h.mu.Unlock()
	return s.accepted
}

// endSession removes s, reporting whether it was still active (so callers notify once).
func (h *Hub) endSession(s *session) bool {
	h.mu.Lock()
	defer h.mu.Unlock()
	return h.endLocked(s)
}

func (h *Hub) endLocked(s *session) bool {
	if h.sessions[s.id] != s {
		return false
	}
	delete(h.sessions, s.id)
	if s.viewer != nil {
		delete(s.viewer.sessions, s.id)
	}
	if s.agent != nil {
		delete(s.agent.sessions, s.id)
	}
	if s.expiry != nil {
		s.expiry.Stop()
	}
	return true
}

// detach unbinds c from all its sessions. Each session is ended after `window` unless the
// side re-attaches; onExpire runs (outside the lock) for each session that expires.
func (h *Hub) detach(c *client, window time.Duration, onExpire func(s *session, other *client)) {
	var ended []*session
	h.mu.Lock()
	defer func() {
		h.mu.Unlock()
		for _, s := range ended {
			onExpire(s, nil) // both sides gone: audit it; nobody to notify
		}
	}()
	for id, s := range c.sessions {
		delete(c.sessions, id)
		if s.viewer == c {
			s.viewer = nil
		}
		if s.agent == c {
			s.agent = nil
		}
		if s.viewer == nil && s.agent == nil {
			if h.endLocked(s) { // nobody left to resume to
				ended = append(ended, s)
			}
			continue
		}
		if s.expiry != nil {
			continue // already counting down for the other side; keep the original deadline
		}
		sess := s
		s.expiry = time.AfterFunc(window, func() {
			h.mu.Lock()
			still := (sess.viewer == nil || sess.agent == nil) && h.endLocked(sess)
			other := sess.viewer
			if other == nil {
				other = sess.agent
			}
			h.mu.Unlock()
			if still {
				onExpire(sess, other)
			}
		})
	}
}

// resume re-attaches c to session sid if it presents its side's token and identity.
func (h *Hub) resume(sid string, c *client, token string) (*session, error) {
	h.mu.Lock()
	defer h.mu.Unlock()
	s := h.sessions[sid]
	if s == nil {
		return nil, store.ErrNotFound
	}
	hash := store.HashToken(token)
	isAgent := c.deviceID != ""
	switch {
	case isAgent && c.deviceID == s.deviceID && subtle.ConstantTimeCompare(hash, s.agentToken) == 1:
		if s.agent != nil && s.agent != c {
			delete(s.agent.sessions, s.id) // stale half-open connection
		}
		s.agent = c
	case !isAgent && c.userID == s.userID && subtle.ConstantTimeCompare(hash, s.viewerToken) == 1:
		if s.viewer != nil && s.viewer != c {
			delete(s.viewer.sessions, s.id)
		}
		s.viewer = c
	default:
		return nil, store.ErrNotFound
	}
	c.sessions[s.id] = s
	if s.viewer != nil && s.agent != nil && s.expiry != nil {
		s.expiry.Stop()
		s.expiry = nil
	}
	return s, nil
}

// ActiveSessions reports the number of live sessions (metrics, tests).
func (h *Hub) ActiveSessions() int {
	h.mu.Lock()
	defer h.mu.Unlock()
	return len(h.sessions)
}

func (h *Handler) send(c *client, t protocol.MessageType, sid, id string, body any) {
	if c == nil {
		return // party detached; it resyncs after resuming
	}
	msg, err := protocol.Reply(t, sid, id, body)
	if err != nil {
		h.Log.Error("marshal message", "type", t, "err", err)
		return
	}
	c.enqueue(msg)
}

// forward relays an envelope's body unchanged to the other party (dropped while detached).
func (h *Handler) forward(to *client, env protocol.Envelope) {
	if to == nil {
		return
	}
	msg, _ := json.Marshal(protocol.Envelope{V: protocol.Version, Type: env.Type, SID: env.SID, Body: env.Body})
	to.enqueue(msg)
}

func (h *Handler) iceServers(sid string) []protocol.ICEServer {
	if h.ICE == nil {
		return []protocol.ICEServer{}
	}
	return h.ICE.ICEServers(sid)
}

func (h *Handler) resumeWindow() time.Duration {
	if h.ResumeWindow > 0 {
		return h.ResumeWindow
	}
	return DefaultResumeWindow
}

// auditSession records a session event with the viewer's IP and browser.
func (h *Handler) auditSession(s *session, action, detail string) {
	h.Audit.Record(store.AuditEvent{
		UserID: s.userID, DeviceID: s.deviceID, SID: s.id, Action: action,
		IP: s.viewerInfo.IP, UserAgent: s.viewerInfo.UserAgent, Detail: detail,
	})
}

// finish ends s (once), audits it, and tells `notify` (if any) why.
func (h *Handler) finish(s *session, by, reason string, notify *client) {
	if !h.Hub.endSession(s) {
		return
	}
	h.send(notify, protocol.TypeSessionEnd, s.id, "", protocol.SessionEnd{Reason: reason})
	h.auditEnd(s, by, reason)
	if s.accepted {
		go h.removeIfOneTime(s.userID, s.deviceID)
	}
}

// removeIfOneTime takes a computer shared "just this once" off the account once its session
// (one that actually connected) is over, and disconnects its agent.
func (h *Handler) removeIfOneTime(userID, deviceID string) {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	d, err := h.Store.GetDevice(ctx, deviceID)
	if err != nil || !d.Temporary || d.RevokedAt != nil {
		return
	}
	if err := h.Store.RevokeDevice(ctx, userID, deviceID, h.Now()); err != nil {
		h.Log.Error("remove one-time device", "device_id", deviceID, "err", err)
		return
	}
	h.Log.Info("one-time device removed after its session", "user_id", userID, "device_id", deviceID)
	h.Audit.Record(store.AuditEvent{UserID: userID, DeviceID: deviceID, Action: store.AuditDeviceRevoked, Detail: "one-time share ended"})
	h.RevokeDevice(deviceID)
}

func (h *Handler) auditEnd(s *session, by, reason string) {
	detail := "reason=" + reason + " by=" + by
	if !s.acceptedAt.IsZero() {
		detail += " duration=" + h.Now().Sub(s.acceptedAt).Round(time.Second).String()
	}
	h.auditSession(s, store.AuditSessionEnded, detail)
	h.Log.Info("session ended", "sid", s.id, "by", by, "reason", reason)
}

// RevokeDevice disconnects a revoked device's agent and ends its sessions immediately.
func (h *Handler) RevokeDevice(deviceID string) {
	for _, s := range h.Hub.deviceSessions(deviceID) {
		viewer, _ := h.Hub.peers(s)
		h.finish(s, "owner", "device_revoked", viewer)
	}
	if c := h.Hub.agentFor(deviceID); c != nil {
		c.close("revoked")
	}
}

// reasonOf extracts the "reason" from a session.end / session.reject body.
func reasonOf(body json.RawMessage) string {
	var r struct {
		Reason string `json:"reason"`
	}
	_ = json.Unmarshal(body, &r)
	if r.Reason == "" || len(r.Reason) > 40 {
		return "unspecified"
	}
	return r.Reason
}

// handleResume processes session.resume from either side.
func (h *Handler) handleResume(c *client, env protocol.Envelope) {
	if h.ResumeLimiter != nil && !h.ResumeLimiter.Allow(c.userID+"/"+c.deviceID) {
		h.send(c, protocol.TypeSessionResume, env.SID, env.ID, protocol.SessionResume{Resumed: false, Reason: protocol.ReasonRateLimited})
		return
	}
	var req protocol.SessionResume
	_ = json.Unmarshal(env.Body, &req)
	s, err := h.Hub.resume(env.SID, c, req.Token)
	if err != nil {
		h.Log.Warn("session resume refused", "sid", env.SID, "user_id", c.userID, "device_id", c.deviceID)
		h.send(c, protocol.TypeSessionResume, env.SID, env.ID, protocol.SessionResume{Resumed: false, Reason: protocol.ReasonResumeFailed})
		return
	}
	side := map[bool]string{true: "agent", false: "viewer"}[c.deviceID != ""]
	h.Log.Info("session resumed", "sid", s.id, "side", side)
	h.auditSession(s, store.AuditSessionResumed, "side="+side)
	h.send(c, protocol.TypeSessionResume, s.id, env.ID, protocol.SessionResume{Resumed: true})
}

// handleViewerSession processes session messages from a viewer.
func (h *Handler) handleViewerSession(c *client, viewer protocol.ViewerInfo, env protocol.Envelope) {
	switch env.Type {
	case protocol.TypeSessionRequest:
		h.startSession(c, viewer, env)
	case protocol.TypeSessionResume:
		h.handleResume(c, env)
	case protocol.TypeSDPAnswer, protocol.TypeICECandidate, protocol.TypeSessionICERestart, protocol.TypeSessionEnd:
		s := h.Hub.sessionFor(env.SID, c)
		if s == nil {
			h.Log.Debug("viewer message for unknown session", "type", env.Type, "sid", env.SID)
			return
		}
		viewerSide, agent := h.Hub.peers(s)
		if viewerSide != c {
			return
		}
		h.forward(agent, env)
		if env.Type == protocol.TypeSessionEnd {
			h.finish(s, "viewer", reasonOf(env.Body), nil)
		}
	default:
		h.Log.Debug("viewer message ignored", "type", env.Type)
	}
}

func (h *Handler) startSession(c *client, viewer protocol.ViewerInfo, env protocol.Envelope) {
	var ownedDevice string // only audited if it is the requester's own device
	reject := func(reason, msg string) {
		h.send(c, protocol.TypeSessionReject, "", env.ID, protocol.SessionReject{Reason: reason, Message: msg})
		h.Audit.Record(store.AuditEvent{
			UserID: c.userID, DeviceID: ownedDevice, Action: store.AuditSessionRejected,
			IP: viewer.IP, UserAgent: viewer.UserAgent, Detail: "reason=" + reason + " by=server",
		})
	}
	if h.SessionLimiter != nil && !h.SessionLimiter.Allow(c.userID) {
		reject(protocol.ReasonRateLimited, "too many connection attempts; wait a minute")
		return
	}
	var req protocol.SessionRequest
	if err := json.Unmarshal(env.Body, &req); err != nil || req.DeviceID == "" {
		reject(protocol.ReasonUnknownDevice, "device_id required")
		return
	}
	// Ownership is checked against the database on every request, not just at connect time.
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	d, err := h.Store.GetDevice(ctx, req.DeviceID)
	if err != nil || d.UserID != c.userID || d.RevokedAt != nil {
		reject(protocol.ReasonUnknownDevice, "no such device on your account")
		return
	}
	ownedDevice = d.ID
	if h.Access != nil {
		if ok, err := h.Access(ctx, c.userID); err != nil || !ok {
			reject(protocol.ReasonPaymentRequired, "get glenley for this account to connect")
			return
		}
	}
	s, viewerToken, agentToken, err := h.Hub.createSession(c, d.ID, viewer, h.Now())
	switch {
	case errors.Is(err, store.ErrNotFound):
		reject(protocol.ReasonDeviceOffline, "the device is offline")
		return
	case errors.Is(err, errBusy):
		reject(protocol.ReasonBusy, "the device is already in a session")
		return
	case errors.Is(err, errTooManySessions):
		reject(protocol.ReasonTooManySessions, "too many open sessions")
		return
	case err != nil:
		reject("internal", "internal error")
		return
	}
	h.Log.Info("session requested", "sid", s.id, "device_id", d.ID, "user_id", c.userID, "viewer_ip", viewer.IP)
	h.auditSession(s, store.AuditSessionRequest, "")
	ice := h.iceServers(s.id)
	_, agent := h.Hub.peers(s)
	// The viewer learns the sid from turn.creds (correlated by its request ID).
	h.send(c, protocol.TypeTURNCreds, s.id, env.ID, protocol.TURNCreds{ICEServers: ice, ResumeToken: viewerToken})
	h.send(agent, protocol.TypeTURNCreds, s.id, "", protocol.TURNCreds{ICEServers: ice, ResumeToken: agentToken})
	h.send(agent, protocol.TypeSessionRequest, s.id, "", protocol.AgentSessionRequest{Viewer: viewer})
}

// handleAgentSession processes session messages from an agent.
func (h *Handler) handleAgentSession(c *client, env protocol.Envelope) {
	if env.Type == protocol.TypeSessionResume {
		h.handleResume(c, env)
		return
	}
	s := h.Hub.sessionFor(env.SID, c)
	if s == nil {
		h.Log.Debug("agent message for unknown session", "type", env.Type, "sid", env.SID)
		return
	}
	viewer, agentSide := h.Hub.peers(s)
	if agentSide != c {
		return
	}
	switch env.Type {
	case protocol.TypeSessionAccept:
		h.Hub.markAccepted(s, h.Now())
		h.auditSession(s, store.AuditSessionAccepted, "")
		h.forward(viewer, env)
	case protocol.TypeSessionReject:
		h.forward(viewer, env)
		if h.Hub.endSession(s) {
			h.auditSession(s, store.AuditSessionRejected, "reason="+reasonOf(env.Body)+" by=host")
		}
	case protocol.TypeSessionEnd:
		h.forward(viewer, env)
		h.finish(s, "host", reasonOf(env.Body), nil)
	case protocol.TypeSDPOffer:
		if !h.Hub.isAccepted(s) {
			h.Log.Warn("offer before accept dropped", "sid", s.id)
			return
		}
		h.forward(viewer, env)
	case protocol.TypeICECandidate:
		h.forward(viewer, env)
	default:
		h.Log.Debug("agent session message ignored", "type", env.Type)
	}
}

// detachAll is called when c's connection closes: its sessions wait for it to resume.
func (h *Handler) detachAll(c *client) {
	h.Hub.detach(c, h.resumeWindow(), func(s *session, other *client) {
		h.send(other, protocol.TypeSessionEnd, s.id, "", protocol.SessionEnd{Reason: protocol.ReasonPeerDisconnected})
		h.auditEnd(s, "timeout", protocol.ReasonPeerDisconnected)
	})
}
