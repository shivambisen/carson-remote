package signaling

import (
	"context"
	"encoding/json"
	"errors"
	"time"

	"carson/server/internal/protocol"
	"carson/server/internal/store"
)

// Limits on concurrent sessions. One viewer per device until multi-viewer support lands.
const maxSessionsPerViewer = 4

var (
	errBusy            = errors.New("device busy")
	errTooManySessions = errors.New("too many sessions")
)

// session pairs one viewer connection with one agent connection. The server only
// relays opaque SDP/ICE between the two; media never touches it.
type session struct {
	id       string
	deviceID string
	viewer   *client
	agent    *client
	accepted bool
}

// createSession registers a session between viewer and the device's current agent.
func (h *Hub) createSession(viewer *client, deviceID string) (*session, error) {
	h.mu.Lock()
	defer h.mu.Unlock()
	agent := h.agents[deviceID]
	if agent == nil {
		return nil, store.ErrNotFound
	}
	if len(viewer.sessions) >= maxSessionsPerViewer {
		return nil, errTooManySessions
	}
	for _, s := range agent.sessions {
		if s.deviceID == deviceID {
			return nil, errBusy
		}
	}
	s := &session{id: store.NewID("ses"), deviceID: deviceID, viewer: viewer, agent: agent}
	h.sessions[s.id] = s
	viewer.sessions[s.id] = s
	agent.sessions[s.id] = s
	return s, nil
}

// sessionFor returns the session sid only if c is one of its two parties.
func (h *Hub) sessionFor(sid string, c *client) *session {
	h.mu.Lock()
	defer h.mu.Unlock()
	s := h.sessions[sid]
	if s == nil || (s.viewer != c && s.agent != c) {
		return nil
	}
	return s
}

func (h *Hub) markAccepted(s *session) {
	h.mu.Lock()
	defer h.mu.Unlock()
	s.accepted = true
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
	if h.sessions[s.id] != s {
		return false
	}
	delete(h.sessions, s.id)
	delete(s.viewer.sessions, s.id)
	delete(s.agent.sessions, s.id)
	return true
}

// clientSessions snapshots c's sessions.
func (h *Hub) clientSessions(c *client) []*session {
	h.mu.Lock()
	defer h.mu.Unlock()
	out := make([]*session, 0, len(c.sessions))
	for _, s := range c.sessions {
		out = append(out, s)
	}
	return out
}

// ActiveSessions reports the number of live sessions (metrics, tests).
func (h *Hub) ActiveSessions() int {
	h.mu.Lock()
	defer h.mu.Unlock()
	return len(h.sessions)
}

func (h *Handler) send(c *client, t protocol.MessageType, sid, id string, body any) {
	msg, err := protocol.Reply(t, sid, id, body)
	if err != nil {
		h.Log.Error("marshal message", "type", t, "err", err)
		return
	}
	c.enqueue(msg)
}

// forward relays an envelope's body unchanged to the other party.
func (h *Handler) forward(to *client, env protocol.Envelope) {
	msg, _ := json.Marshal(protocol.Envelope{V: protocol.Version, Type: env.Type, SID: env.SID, Body: env.Body})
	to.enqueue(msg)
}

func (h *Handler) iceServers() protocol.TURNCreds {
	creds := protocol.TURNCreds{ICEServers: []protocol.ICEServer{}}
	if len(h.STUNURLs) > 0 {
		creds.ICEServers = append(creds.ICEServers, protocol.ICEServer{URLs: h.STUNURLs})
	}
	return creds
}

// handleViewerSession processes session messages from a viewer.
func (h *Handler) handleViewerSession(c *client, viewer protocol.ViewerInfo, env protocol.Envelope) {
	switch env.Type {
	case protocol.TypeSessionRequest:
		h.startSession(c, viewer, env)
	case protocol.TypeSDPAnswer, protocol.TypeICECandidate, protocol.TypeSessionEnd:
		s := h.Hub.sessionFor(env.SID, c)
		if s == nil || s.viewer != c {
			h.Log.Debug("viewer message for unknown session", "type", env.Type, "sid", env.SID)
			return
		}
		h.forward(s.agent, env)
		if env.Type == protocol.TypeSessionEnd {
			h.Hub.endSession(s)
			h.Log.Info("session ended", "sid", s.id, "by", "viewer")
		}
	default:
		h.Log.Debug("viewer message ignored", "type", env.Type)
	}
}

func (h *Handler) startSession(c *client, viewer protocol.ViewerInfo, env protocol.Envelope) {
	reject := func(reason, msg string) {
		h.send(c, protocol.TypeSessionReject, "", env.ID, protocol.SessionReject{Reason: reason, Message: msg})
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
	s, err := h.Hub.createSession(c, d.ID)
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
	creds := h.iceServers()
	// The viewer learns the sid from turn.creds (correlated by its request ID).
	h.send(c, protocol.TypeTURNCreds, s.id, env.ID, creds)
	h.send(s.agent, protocol.TypeTURNCreds, s.id, "", creds)
	h.send(s.agent, protocol.TypeSessionRequest, s.id, "", protocol.AgentSessionRequest{Viewer: viewer})
}

// handleAgentSession processes session messages from an agent.
func (h *Handler) handleAgentSession(c *client, env protocol.Envelope) {
	s := h.Hub.sessionFor(env.SID, c)
	if s == nil || s.agent != c {
		h.Log.Debug("agent message for unknown session", "type", env.Type, "sid", env.SID)
		return
	}
	switch env.Type {
	case protocol.TypeSessionAccept:
		h.Hub.markAccepted(s)
		h.forward(s.viewer, env)
	case protocol.TypeSessionReject, protocol.TypeSessionEnd:
		h.forward(s.viewer, env)
		h.Hub.endSession(s)
		h.Log.Info("session ended", "sid", s.id, "by", "agent", "type", env.Type)
	case protocol.TypeSDPOffer:
		if !h.Hub.isAccepted(s) {
			h.Log.Warn("offer before accept dropped", "sid", s.id)
			return
		}
		h.forward(s.viewer, env)
	case protocol.TypeICECandidate:
		h.forward(s.viewer, env)
	default:
		h.Log.Debug("agent session message ignored", "type", env.Type)
	}
}

// endAllSessions tears down c's sessions and tells the other side.
func (h *Handler) endAllSessions(c *client) {
	for _, s := range h.Hub.clientSessions(c) {
		if !h.Hub.endSession(s) {
			continue
		}
		other := s.agent
		if other == c {
			other = s.viewer
		}
		h.send(other, protocol.TypeSessionEnd, s.id, "", protocol.SessionEnd{Reason: protocol.ReasonPeerDisconnected})
		h.Log.Info("session ended", "sid", s.id, "by", "disconnect")
	}
}
