package signaling

import (
	"context"
	"encoding/json"
	"strings"
	"testing"
	"time"

	"github.com/coder/websocket"

	"carson/server/internal/protocol"
	"carson/server/internal/store"
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
	agentToken    string // resume tokens from the last request()
	viewerToken   string
	agentDevToken string // device access token, to reconnect the agent
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
	return &sessionEnv{env: e, viewer: newPeer(viewer), agent: newPeer(agent), deviceID: devID, agentDevToken: tok}
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
// viewerUser returns the viewer's user ID (ana@x.com).
func (s *sessionEnv) viewerUser(t *testing.T) string {
	t.Helper()
	u, err := s.store.UpsertUserForIdentity(context.Background(), store.Identity{Provider: "dev", Subject: "ana@x.com", Email: "ana@x.com"}, time.Now())
	if err != nil {
		t.Fatal(err)
	}
	return u.ID
}

func (s *sessionEnv) request(t *testing.T) string {
	t.Helper()
	sendEnv(t, s.viewer, protocol.TypeSessionRequest, "", "req-1", protocol.SessionRequest{DeviceID: s.deviceID})
	creds := readType(t, s.viewer, protocol.TypeTURNCreds)
	if creds.ID != "req-1" || creds.SID == "" {
		t.Fatalf("viewer turn.creds = %+v", creds)
	}
	agentCreds := readType(t, s.agent, protocol.TypeTURNCreds)
	if agentCreds.SID != creds.SID {
		t.Fatalf("agent turn.creds sid = %q", agentCreds.SID)
	}
	var vc, ac protocol.TURNCreds
	_ = json.Unmarshal(creds.Body, &vc)
	_ = json.Unmarshal(agentCreds.Body, &ac)
	if vc.ResumeToken == "" || ac.ResumeToken == "" || vc.ResumeToken == ac.ResumeToken {
		t.Fatalf("each party needs its own resume token: %q %q", vc.ResumeToken, ac.ResumeToken)
	}
	s.viewerToken, s.agentToken = vc.ResumeToken, ac.ResumeToken
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

func TestSessionEndsWhenDroppedPartyDoesNotResume(t *testing.T) {
	s := newSessionEnv(t)
	sid := s.request(t)
	start := time.Now()
	s.agent.conn.Close(websocket.StatusNormalClosure, "bye")
	expectSilence(t, s.viewer, 150*time.Millisecond) // still within the resume window
	if s.hub.ActiveSessions() != 1 {
		t.Fatal("session must survive a signaling drop for the resume window")
	}
	env := readType(t, s.viewer, protocol.TypeSessionEnd)
	var end protocol.SessionEnd
	_ = json.Unmarshal(env.Body, &end)
	if env.SID != sid || end.Reason != protocol.ReasonPeerDisconnected || time.Since(start) < 250*time.Millisecond {
		t.Fatalf("session.end = %+v %+v after %s", env, end, time.Since(start))
	}
	if s.hub.ActiveSessions() != 0 {
		t.Fatal("session not cleaned up")
	}
}

func resumeOn(t *testing.T, p *peer, sid, token string) protocol.SessionResume {
	t.Helper()
	sendEnv(t, p, protocol.TypeSessionResume, sid, "r1", protocol.SessionResume{Token: token})
	env := readType(t, p, protocol.TypeSessionResume)
	var r protocol.SessionResume
	_ = json.Unmarshal(env.Body, &r)
	return r
}

func TestAgentResumesAfterSignalingDrop(t *testing.T) {
	s := newSessionEnv(t)
	sid := s.request(t)
	sendEnv(t, s.agent, protocol.TypeSessionAccept, sid, "", map[string]any{})
	readType(t, s.viewer, protocol.TypeSessionAccept)

	s.agent.conn.Close(websocket.StatusNormalClosure, "network change")
	agent2, _, err := s.dialAgent(t, s.agentDevToken)
	if err != nil {
		t.Fatal(err)
	}
	a2 := newPeer(agent2)
	if r := resumeOn(t, a2, sid, "wrong-token"); r.Resumed {
		t.Fatal("resume with a wrong token must fail")
	}
	if r := resumeOn(t, a2, sid, s.agentToken); !r.Resumed {
		t.Fatalf("resume failed: %+v", r)
	}
	// Relaying works again in both directions, and the session outlives the window.
	sendEnv(t, s.viewer, protocol.TypeSessionICERestart, sid, "", map[string]any{})
	readType(t, a2, protocol.TypeSessionICERestart)
	sendEnv(t, a2, protocol.TypeSDPOffer, sid, "", map[string]string{"type": "offer", "sdp": "restart"})
	readType(t, s.viewer, protocol.TypeSDPOffer)
	time.Sleep(400 * time.Millisecond)
	if s.hub.ActiveSessions() != 1 {
		t.Fatal("resumed session was expired")
	}
	expectSilence(t, s.viewer, 50*time.Millisecond)
}

func TestViewerResumeRequiresSameAccountAndOwnToken(t *testing.T) {
	s := newSessionEnv(t)
	sid := s.request(t)
	s.viewer.conn.Close(websocket.StatusNormalClosure, "tab reload")

	_, boCookie := s.session(t, "bo@x.com")
	boConn, _, _ := s.dialViewer(t, boCookie, s.srv.URL)
	bo := newPeer(boConn)
	if r := resumeOn(t, bo, sid, s.viewerToken); r.Resumed {
		t.Fatal("another account must not resume with a stolen token")
	}

	_, anaCookie := s.session(t, "ana@x.com")
	anaConn, _, _ := s.dialViewer(t, anaCookie, s.srv.URL)
	ana := newPeer(anaConn)
	if r := resumeOn(t, ana, sid, s.agentToken); r.Resumed {
		t.Fatal("the agent's token must not resume the viewer side")
	}
	if r := resumeOn(t, ana, sid, s.viewerToken); !r.Resumed {
		t.Fatal("same account with its own token must resume")
	}
	sendEnv(t, s.agent, protocol.TypeICECandidate, sid, "", map[string]string{"candidate": "c"})
	readType(t, ana, protocol.TypeICECandidate)
}

func TestDeviceStaysBusyWhileItsSessionAwaitsResume(t *testing.T) {
	s := newSessionEnv(t)
	s.request(t)
	s.agent.conn.Close(websocket.StatusNormalClosure, "drop")
	agent2, _, _ := s.dialAgent(t, s.agentDevToken)
	newPeer(agent2)
	waitOnline(t, s.hub, s.deviceID)
	sendEnv(t, s.viewer, protocol.TypeSessionRequest, "", "r2", protocol.SessionRequest{DeviceID: s.deviceID})
	env := readType(t, s.viewer, protocol.TypeSessionReject)
	var rej protocol.SessionReject
	_ = json.Unmarshal(env.Body, &rej)
	if rej.Reason != protocol.ReasonBusy {
		t.Fatalf("reject = %+v", rej)
	}
}

func TestTurnCredsCarryTimeLimitedTURN(t *testing.T) {
	s := newSessionEnv(t)
	sendEnv(t, s.viewer, protocol.TypeSessionRequest, "", "rq", protocol.SessionRequest{DeviceID: s.deviceID})
	env := readType(t, s.viewer, protocol.TypeTURNCreds)
	var c protocol.TURNCreds
	_ = json.Unmarshal(env.Body, &c)
	if len(c.ICEServers) != 2 || c.ICEServers[1].Username == "" || !strings.HasSuffix(c.ICEServers[1].Username, ":"+env.SID) {
		t.Fatalf("ice servers = %+v", c.ICEServers)
	}
}

func TestSessionLifecycleIsAudited(t *testing.T) {
	s := newSessionEnv(t)
	sid := s.request(t)
	sendEnv(t, s.agent, protocol.TypeSessionAccept, sid, "", map[string]any{})
	readType(t, s.viewer, protocol.TypeSessionAccept)
	sendEnv(t, s.viewer, protocol.TypeSessionEnd, sid, "", protocol.SessionEnd{Reason: "user"})
	readType(t, s.agent, protocol.TypeSessionEnd)

	events, err := s.store.ListAudit(context.Background(), s.viewerUser(t), 0, 10)
	if err != nil || len(events) != 3 {
		t.Fatalf("events = %+v %v", events, err)
	}
	// Newest first.
	if events[2].Action != store.AuditSessionRequest || events[1].Action != store.AuditSessionAccepted ||
		events[0].Action != store.AuditSessionEnded || !strings.HasPrefix(events[0].Detail, "reason=user by=viewer duration=") {
		t.Fatalf("audit = %+v", events)
	}
	for _, e := range events {
		if e.SID != sid || e.IP == "" || e.DeviceName != "Mac" {
			t.Fatalf("audit row missing who/where: %+v", e)
		}
	}
}

func TestRevokeEndsSessionAndKicksAgent(t *testing.T) {
	s := newSessionEnv(t)
	sid := s.request(t)
	s.handler.RevokeDevice(s.deviceID)

	env := readType(t, s.viewer, protocol.TypeSessionEnd)
	var end protocol.SessionEnd
	_ = json.Unmarshal(env.Body, &end)
	if env.SID != sid || end.Reason != "device_revoked" {
		t.Fatalf("viewer got %+v %+v", env, end)
	}
	select {
	case _, ok := <-s.agent.msgs:
		for ok {
			_, ok = <-s.agent.msgs
		}
	case <-time.After(2 * time.Second):
		t.Fatal("agent was not disconnected")
	}
	deadline := time.Now().Add(2 * time.Second)
	for s.hub.Online(s.deviceID) {
		if time.Now().After(deadline) {
			t.Fatal("revoked device still online")
		}
		time.Sleep(10 * time.Millisecond)
	}
	events, _ := s.store.ListAudit(context.Background(), s.viewerUser(t), 0, 1)
	if len(events) != 1 || events[0].Detail != "reason=device_revoked by=owner" {
		t.Fatalf("revocation not audited: %+v", events)
	}
}

func TestFloodingConnectionIsClosed(t *testing.T) {
	s := newSessionEnv(t)
	for i := range 200 {
		msg, _ := protocol.Reply(protocol.TypeICECandidate, "ses_nope", "", map[string]int{"n": i})
		if err := s.viewer.conn.Write(context.Background(), websocket.MessageText, msg); err != nil {
			break // closed mid-flood: expected
		}
	}
	deadline := time.After(2 * time.Second)
	for {
		select {
		case _, ok := <-s.viewer.msgs:
			if !ok {
				return // closed
			}
		case <-deadline:
			t.Fatal("flooding connection was not closed")
		}
	}
}

func TestResumeAttemptsAreRateLimited(t *testing.T) {
	s := newSessionEnv(t)
	sid := s.request(t)
	var last protocol.SessionResume
	for range 4 {
		last = resumeOn(t, s.viewer, sid, "guess")
	}
	if last.Resumed || last.Reason != protocol.ReasonRateLimited {
		t.Fatalf("4th attempt = %+v", last)
	}
}

func TestServerRejectionsAreAuditedWithoutLeakingOthersDevices(t *testing.T) {
	s := newSessionEnv(t)
	bo, boCookie := s.session(t, "bo@x.com")
	boConn, _, _ := s.dialViewer(t, boCookie, s.srv.URL)
	p := newPeer(boConn)
	sendEnv(t, p, protocol.TypeSessionRequest, "", "r", protocol.SessionRequest{DeviceID: s.deviceID})
	readType(t, p, protocol.TypeSessionReject)
	events, _ := s.store.ListAudit(context.Background(), bo.ID, 0, 5)
	if len(events) != 1 || events[0].Action != store.AuditSessionRejected || events[0].DeviceID != "" || events[0].DeviceName != "" {
		t.Fatalf("bo's audit = %+v", events)
	}
}

func TestUnpaidAccountsCannotConnect(t *testing.T) {
	s := newSessionEnv(t)
	s.handler.Access = func(context.Context, string) (bool, error) { return false, nil }
	sendEnv(t, s.viewer, protocol.TypeSessionRequest, "", "r", protocol.SessionRequest{DeviceID: s.deviceID})
	env := readType(t, s.viewer, protocol.TypeSessionReject)
	var rej protocol.SessionReject
	_ = json.Unmarshal(env.Body, &rej)
	if rej.Reason != protocol.ReasonPaymentRequired {
		t.Fatalf("reject = %+v", rej)
	}
	expectSilence(t, s.agent, 150*time.Millisecond) // the computer is never contacted
}

func TestOneTimeDeviceLeavesAfterItsSession(t *testing.T) {
	s := newSessionEnv(t)
	ctx := context.Background()
	// Mark the paired device as shared "just this once".
	if _, err := s.store.DB().ExecContext(ctx, `UPDATE devices SET temporary = 1 WHERE id = ?`, s.deviceID); err != nil {
		t.Fatal(err)
	}
	sid := s.request(t)
	sendEnv(t, s.agent, protocol.TypeSessionAccept, sid, "", map[string]any{})
	readType(t, s.viewer, protocol.TypeSessionAccept)
	sendEnv(t, s.viewer, protocol.TypeSessionEnd, sid, "", protocol.SessionEnd{Reason: "user"})
	readType(t, s.agent, protocol.TypeSessionEnd)

	deadline := time.Now().Add(3 * time.Second)
	for {
		d, err := s.store.GetDevice(ctx, s.deviceID)
		if err == nil && d.RevokedAt != nil {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("one-time device still on the account after its session")
		}
		time.Sleep(20 * time.Millisecond)
	}
	// The agent is disconnected and can't come back.
	if s.hub.Online(s.deviceID) {
		time.Sleep(200 * time.Millisecond)
		if s.hub.Online(s.deviceID) {
			t.Fatal("one-time agent still online")
		}
	}
}
