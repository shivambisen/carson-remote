package signaling

import (
	"context"
	"encoding/json"
	"testing"
	"time"

	"github.com/coder/websocket"

	"carson/server/internal/protocol"
)

// peer wraps a test connection with a background reader. coder/websocket closes a
// connection whose Read context expires, so tests must never time out a Read directly.
type peer struct {
	conn *websocket.Conn
	msgs chan protocol.Envelope
}

func newPeer(c *websocket.Conn) *peer {
	p := &peer{conn: c, msgs: make(chan protocol.Envelope, 64)}
	go func() {
		defer close(p.msgs)
		for {
			_, data, err := c.Read(context.Background())
			if err != nil {
				return
			}
			var env protocol.Envelope
			if json.Unmarshal(data, &env) == nil {
				p.msgs <- env
			}
		}
	}()
	return p
}

func sendEnv(t *testing.T, p *peer, typ protocol.MessageType, sid, id string, body any) {
	t.Helper()
	msg, _ := protocol.Reply(typ, sid, id, body)
	if err := p.conn.Write(context.Background(), websocket.MessageText, msg); err != nil {
		t.Fatal(err)
	}
}

// readType waits for a message of type want, skipping presence noise.
func readType(t *testing.T, p *peer, want protocol.MessageType) protocol.Envelope {
	t.Helper()
	timeout := time.After(2 * time.Second)
	for {
		select {
		case env, ok := <-p.msgs:
			if !ok {
				t.Fatalf("connection closed while waiting for %s", want)
			}
			if env.Type == want {
				return env
			}
			if env.Type != protocol.TypePresence && env.Type != protocol.TypeHelloOK {
				t.Fatalf("got %s (%s), want %s", env.Type, env.Body, want)
			}
		case <-timeout:
			t.Fatalf("timed out waiting for %s", want)
		}
	}
}

// expectSilence asserts nothing but presence arrives within d.
func expectSilence(t *testing.T, p *peer, d time.Duration) {
	t.Helper()
	timeout := time.After(d)
	for {
		select {
		case env, ok := <-p.msgs:
			if ok && env.Type != protocol.TypePresence {
				t.Fatalf("unexpected message: %s %s", env.Type, env.Body)
			}
		case <-timeout:
			return
		}
	}
}

type sessionEnv struct {
	*env
	viewer, agent *peer
	deviceID      string
}

// newSessionEnv connects one agent and one viewer of the same user.
func newSessionEnv(t *testing.T) *sessionEnv {
	t.Helper()
	// Session tests read sockets only when expecting messages, so use real-world heartbeats.
	e := newEnvWithHeartbeat(t, time.Minute, time.Minute)
	u, cookie := e.session(t, "ana@x.com")
	devID, tok := e.device(t, u)
	agent, _, err := e.dialAgent(t, tok)
	if err != nil {
		t.Fatal(err)
	}
	waitOnline(t, e.hub, devID)
	viewer, _, err := e.dialViewer(t, cookie, e.srv.URL)
	if err != nil {
		t.Fatal(err)
	}
	return &sessionEnv{env: e, viewer: newPeer(viewer), agent: newPeer(agent), deviceID: devID}
}

func waitOnline(t *testing.T, h *Hub, id string) {
	t.Helper()
	deadline := time.Now().Add(2 * time.Second)
	for !h.Online(id) {
		if time.Now().After(deadline) {
			t.Fatal("device never came online")
		}
		time.Sleep(5 * time.Millisecond)
	}
}

// request starts a session and returns its sid as seen by the viewer and agent.
func (s *sessionEnv) request(t *testing.T) string {
	t.Helper()
	sendEnv(t, s.viewer, protocol.TypeSessionRequest, "", "req-1", protocol.SessionRequest{DeviceID: s.deviceID})
	creds := readType(t, s.viewer, protocol.TypeTURNCreds)
	if creds.ID != "req-1" || creds.SID == "" {
		t.Fatalf("viewer turn.creds = %+v", creds)
	}
	if got := readType(t, s.agent, protocol.TypeTURNCreds); got.SID != creds.SID {
		t.Fatalf("agent turn.creds sid = %q", got.SID)
	}
	req := readType(t, s.agent, protocol.TypeSessionRequest)
	var body protocol.AgentSessionRequest
	_ = json.Unmarshal(req.Body, &body)
	if req.SID != creds.SID || body.Viewer.Email != "ana@x.com" || body.Viewer.IP == "" {
		t.Fatalf("agent session.request = %+v %+v", req, body)
	}
	return creds.SID
}

func TestSessionRelayHappyPath(t *testing.T) {
	s := newSessionEnv(t)
	sid := s.request(t)

	sendEnv(t, s.agent, protocol.TypeSessionAccept, sid, "", map[string]any{})
	readType(t, s.viewer, protocol.TypeSessionAccept)

	offer := map[string]string{"type": "offer", "sdp": "v=0..."}
	sendEnv(t, s.agent, protocol.TypeSDPOffer, sid, "", offer)
	got := readType(t, s.viewer, protocol.TypeSDPOffer)
	var gotOffer map[string]string
	_ = json.Unmarshal(got.Body, &gotOffer)
	if got.SID != sid || gotOffer["sdp"] != "v=0..." {
		t.Fatalf("offer relayed as %+v", got)
	}

	sendEnv(t, s.viewer, protocol.TypeSDPAnswer, sid, "", map[string]string{"type": "answer", "sdp": "v=0 answer"})
	readType(t, s.agent, protocol.TypeSDPAnswer)
	sendEnv(t, s.viewer, protocol.TypeICECandidate, sid, "", map[string]string{"candidate": "candidate:1 1 udp ..."})
	readType(t, s.agent, protocol.TypeICECandidate)
	sendEnv(t, s.agent, protocol.TypeICECandidate, sid, "", map[string]string{"candidate": "candidate:2 1 udp ..."})
	readType(t, s.viewer, protocol.TypeICECandidate)

	sendEnv(t, s.viewer, protocol.TypeSessionEnd, sid, "", protocol.SessionEnd{Reason: "user"})
	readType(t, s.agent, protocol.TypeSessionEnd)
	if n := s.hub.ActiveSessions(); n != 0 {
		t.Fatalf("active sessions = %d", n)
	}
}

func TestSessionRequestRejections(t *testing.T) {
	s := newSessionEnv(t)
	reject := func(c *peer, deviceID string) protocol.SessionReject {
		t.Helper()
		sendEnv(t, c, protocol.TypeSessionRequest, "", "r", protocol.SessionRequest{DeviceID: deviceID})
		env := readType(t, c, protocol.TypeSessionReject)
		var rej protocol.SessionReject
		_ = json.Unmarshal(env.Body, &rej)
		if env.ID != "r" {
			t.Fatalf("reject not correlated: %+v", env)
		}
		return rej
	}

	if r := reject(s.viewer, "dev_doesnotexist"); r.Reason != protocol.ReasonUnknownDevice {
		t.Errorf("unknown device: %+v", r)
	}

	// Another account cannot even learn whether the device is online.
	_, boCookie := s.session(t, "bo@x.com")
	boConn, _, _ := s.dialViewer(t, boCookie, s.srv.URL)
	bo := newPeer(boConn)
	if r := reject(bo, s.deviceID); r.Reason != protocol.ReasonUnknownDevice {
		t.Errorf("cross-account: %+v", r)
	}
	expectSilence(t, s.agent, 150*time.Millisecond)

	// Busy while a session is open.
	s.request(t)
	if r := reject(s.viewer, s.deviceID); r.Reason != protocol.ReasonBusy {
		t.Errorf("busy: %+v", r)
	}

	// Offline device.
	u, _ := s.session(t, "ana@x.com")
	offlineID, _ := s.device(t, u)
	if r := reject(s.viewer, offlineID); r.Reason != protocol.ReasonDeviceOffline {
		t.Errorf("offline: %+v", r)
	}
}

func TestOnlySessionPartiesCanRelay(t *testing.T) {
	s := newSessionEnv(t)
	sid := s.request(t)
	sendEnv(t, s.agent, protocol.TypeSessionAccept, sid, "", map[string]any{})
	readType(t, s.viewer, protocol.TypeSessionAccept)

	// A second viewer of the SAME account is still not a party to this session.
	_, cookie := s.session(t, "ana@x.com")
	otherConn, _, _ := s.dialViewer(t, cookie, s.srv.URL)
	other := newPeer(otherConn)
	readType(t, other, protocol.TypeHelloOK)
	sendEnv(t, other, protocol.TypeSDPAnswer, sid, "", map[string]string{"sdp": "evil"})
	sendEnv(t, other, protocol.TypeICECandidate, sid, "", map[string]string{"candidate": "evil"})
	sendEnv(t, other, protocol.TypeSessionEnd, sid, "", nil)
	expectSilence(t, s.agent, 150*time.Millisecond)
	if s.hub.ActiveSessions() != 1 {
		t.Fatal("non-party ended the session")
	}

	// The viewer cannot impersonate the agent's side (offers only flow agent → viewer).
	sendEnv(t, s.viewer, protocol.TypeSDPOffer, sid, "", map[string]string{"sdp": "x"})
	expectSilence(t, s.agent, 150*time.Millisecond)
}

func TestOfferBeforeAcceptIsDropped(t *testing.T) {
	s := newSessionEnv(t)
	sid := s.request(t)
	sendEnv(t, s.agent, protocol.TypeSDPOffer, sid, "", map[string]string{"sdp": "early"})
	expectSilence(t, s.viewer, 150*time.Millisecond)
}

func TestAgentRejectEndsSession(t *testing.T) {
	s := newSessionEnv(t)
	sid := s.request(t)
	sendEnv(t, s.agent, protocol.TypeSessionReject, sid, "", protocol.SessionReject{Reason: "permission_denied"})
	env := readType(t, s.viewer, protocol.TypeSessionReject)
	var rej protocol.SessionReject
	_ = json.Unmarshal(env.Body, &rej)
	if env.SID != sid || rej.Reason != "permission_denied" {
		t.Fatalf("reject = %+v %+v", env, rej)
	}
	if s.hub.ActiveSessions() != 0 {
		t.Fatal("rejected session still active")
	}
}

func TestDisconnectEndsSessionForOtherParty(t *testing.T) {
	s := newSessionEnv(t)
	sid := s.request(t)
	s.agent.conn.Close(websocket.StatusNormalClosure, "bye")
	env := readType(t, s.viewer, protocol.TypeSessionEnd)
	var end protocol.SessionEnd
	_ = json.Unmarshal(env.Body, &end)
	if env.SID != sid || end.Reason != protocol.ReasonPeerDisconnected {
		t.Fatalf("session.end = %+v %+v", env, end)
	}
	if s.hub.ActiveSessions() != 0 {
		t.Fatal("session not cleaned up")
	}
}
