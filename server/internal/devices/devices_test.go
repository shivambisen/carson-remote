package devices

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"encoding/hex"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"net/http/cookiejar"
	"net/http/httptest"
	"net/url"
	"os"
	"regexp"
	"slices"
	"sync"
	"testing"
	"time"

	"carson/server/internal/audit"
	"carson/server/internal/auth"
	"carson/server/internal/ratelimit"
	"carson/server/internal/store"
	"carson/server/internal/store/storetest"
)

type fakePresence struct{ online map[string]bool }

func (f fakePresence) Online(id string) bool { return f.online[id] }

type clock struct {
	mu sync.Mutex
	t  time.Time
}

func (c *clock) Now() time.Time          { c.mu.Lock(); defer c.mu.Unlock(); return c.t }
func (c *clock) Advance(d time.Duration) { c.mu.Lock(); c.t = c.t.Add(d); c.mu.Unlock() }

type env struct {
	srv   *httptest.Server
	svc   *Service
	clock *clock
	pres  fakePresence
}

func newEnv(t *testing.T) *env {
	t.Helper()
	st := storetest.Open(t)
	e := &env{clock: &clock{t: time.Now()}, pres: fakePresence{online: map[string]bool{}}}
	mux := http.NewServeMux()
	e.srv = httptest.NewServer(mux)
	t.Cleanup(e.srv.Close)
	pub, _ := url.Parse(e.srv.URL)
	log := slog.New(slog.NewTextHandler(io.Discard, nil))
	lim := func() *ratelimit.Limiter { return ratelimit.New(1000, time.Minute, 1000) }
	a := &auth.Service{Store: st, PublicURL: pub, Dev: true, Log: log, Now: e.clock.Now, LoginLimiter: lim(), Providers: map[string]auth.Provider{}}
	a.Routes(mux)
	e.svc = &Service{
		Store: st, Auth: a, Presence: e.pres, Log: log, Now: e.clock.Now,
		Limits: Limiters{PairStart: lim(), PairCode: ratelimit.New(5, time.Minute, 5), PairPoll: lim(), DeviceAuth: lim()},
	}
	e.svc.Routes(mux)
	return e
}

// user returns a client signed in (via dev login) as email.
func (e *env) user(t *testing.T, email string) *http.Client {
	t.Helper()
	jar, _ := cookiejar.New(nil)
	c := &http.Client{Jar: jar}
	if code, _ := e.call(t, c, "/auth/dev/login", map[string]string{"email": email}, nil); code != http.StatusOK {
		t.Fatalf("dev login = %d", code)
	}
	return c
}

func (e *env) call(t *testing.T, c *http.Client, path string, body any, out any) (int, string) {
	t.Helper()
	if c == nil {
		c = http.DefaultClient
	}
	b, _ := json.Marshal(body)
	req, _ := http.NewRequest(http.MethodPost, e.srv.URL+path, bytes.NewReader(b))
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Origin", e.srv.URL)
	resp, err := c.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	raw, _ := io.ReadAll(resp.Body)
	if out != nil {
		_ = json.Unmarshal(raw, out)
	}
	return resp.StatusCode, string(raw)
}

type startResp struct {
	PairingID string `json:"pairing_id"`
	UserCode  string `json:"user_code"`
	Interval  int    `json:"interval"`
}

func (e *env) startPairing(t *testing.T, pub ed25519.PublicKey) startResp {
	t.Helper()
	var s startResp
	if code, body := e.call(t, nil, "/v1/pair/start", map[string]string{"public_key": b64.EncodeToString(pub), "name": "Ana's MacBook", "os": "macos"}, &s); code != http.StatusOK {
		t.Fatalf("pair/start = %d %s", code, body)
	}
	return s
}

// enroll runs the full pairing flow for a new key and returns it with the device ID.
func (e *env) enroll(t *testing.T, owner *http.Client) (ed25519.PrivateKey, string) {
	t.Helper()
	pub, priv, _ := ed25519.GenerateKey(nil)
	s := e.startPairing(t, pub)
	if code, body := e.call(t, owner, "/v1/pair/approve", map[string]string{"code": s.UserCode}, nil); code != http.StatusOK {
		t.Fatalf("approve = %d %s", code, body)
	}
	var poll struct {
		Status   string `json:"status"`
		DeviceID string `json:"device_id"`
	}
	e.call(t, nil, "/v1/pair/poll", map[string]string{"pairing_id": s.PairingID}, &poll)
	return priv, poll.DeviceID
}

func (e *env) token(t *testing.T, key ed25519.PrivateKey, deviceID string) (int, string) {
	t.Helper()
	var ch struct{ Nonce string }
	e.call(t, nil, "/v1/device/challenge", map[string]string{"device_id": deviceID}, &ch)
	sig := ed25519.Sign(key, AuthMessage(deviceID, ch.Nonce))
	var tok struct {
		AccessToken string `json:"access_token"`
	}
	code, _ := e.call(t, nil, "/v1/device/token", map[string]string{"device_id": deviceID, "nonce": ch.Nonce, "signature": b64.EncodeToString(sig)}, &tok)
	return code, tok.AccessToken
}

func TestUserCodeFormat(t *testing.T) {
	re := regexp.MustCompile(`^[BCDFGHJKLMNPQRSTVWXZ]{4}-[BCDFGHJKLMNPQRSTVWXZ]{4}$`)
	for range 100 {
		if c := NewUserCode(); !re.MatchString(c) {
			t.Fatalf("bad code %q", c)
		}
	}
	if NormalizeCode("bcdf ghjk") != NormalizeCode("BCDF-GHJK") {
		t.Fatal("normalization must ignore case, spaces and dashes")
	}
}

func TestPairingFlowAndDeviceAuth(t *testing.T) {
	e := newEnv(t)
	ana := e.user(t, "ana@x.com")
	pub, priv, _ := ed25519.GenerateKey(nil)
	s := e.startPairing(t, pub)
	if s.Interval != 5 {
		t.Fatalf("interval = %d", s.Interval)
	}

	var poll struct {
		Status   string `json:"status"`
		DeviceID string `json:"device_id"`
	}
	e.call(t, nil, "/v1/pair/poll", map[string]string{"pairing_id": s.PairingID}, &poll)
	if poll.Status != "pending" {
		t.Fatalf("poll before approve = %+v", poll)
	}

	var look struct{ Name, OS string }
	if code, _ := e.call(t, ana, "/v1/pair/lookup", map[string]string{"code": s.UserCode}, &look); code != http.StatusOK || look.Name != "Ana's MacBook" || look.OS != "macos" {
		t.Fatalf("lookup = %d %+v", code, look)
	}
	// Approve with a lower-case, undashed code: users type codes sloppily.
	if code, body := e.call(t, ana, "/v1/pair/approve", map[string]string{"code": NormalizeCode(s.UserCode)[:4] + " " + NormalizeCode(s.UserCode)[4:]}, nil); code != http.StatusOK {
		t.Fatalf("approve = %d %s", code, body)
	}
	e.call(t, nil, "/v1/pair/poll", map[string]string{"pairing_id": s.PairingID}, &poll)
	if poll.Status != "approved" || poll.DeviceID == "" {
		t.Fatalf("poll after approve = %+v", poll)
	}

	code, token := e.token(t, priv, poll.DeviceID)
	if code != http.StatusOK || token == "" {
		t.Fatalf("token = %d", code)
	}
	req := httptest.NewRequest("GET", "/", nil)
	req.Header.Set("Authorization", "Bearer "+token)
	d, err := e.svc.DeviceFromRequest(context.Background(), req)
	if err != nil || d.ID != poll.DeviceID {
		t.Fatalf("DeviceFromRequest = %+v %v", d, err)
	}
	e.clock.Advance(AccessTokenTTL)
	if _, err := e.svc.DeviceFromRequest(context.Background(), req); err == nil {
		t.Fatal("expired access token accepted")
	}
}

func TestDeviceAuthRejectsBadProofs(t *testing.T) {
	e := newEnv(t)
	ana := e.user(t, "ana@x.com")
	priv, id := e.enroll(t, ana)
	_, otherPriv, _ := ed25519.GenerateKey(nil)

	if code, _ := e.token(t, otherPriv, id); code != http.StatusUnauthorized {
		t.Errorf("wrong key: %d", code)
	}
	// Nonce is single-use and bound to the device it was issued for.
	var ch struct{ Nonce string }
	e.call(t, nil, "/v1/device/challenge", map[string]string{"device_id": id}, &ch)
	sig := b64.EncodeToString(ed25519.Sign(priv, AuthMessage(id, ch.Nonce)))
	if code, _ := e.call(t, nil, "/v1/device/token", map[string]string{"device_id": id, "nonce": ch.Nonce, "signature": sig}, nil); code != http.StatusOK {
		t.Fatalf("first use: %d", code)
	}
	if code, _ := e.call(t, nil, "/v1/device/token", map[string]string{"device_id": id, "nonce": ch.Nonce, "signature": sig}, nil); code != http.StatusUnauthorized {
		t.Errorf("replayed nonce: %d", code)
	}
	e.call(t, nil, "/v1/device/challenge", map[string]string{"device_id": "dev_other"}, &ch)
	sig = b64.EncodeToString(ed25519.Sign(priv, AuthMessage(id, ch.Nonce)))
	if code, _ := e.call(t, nil, "/v1/device/token", map[string]string{"device_id": id, "nonce": ch.Nonce, "signature": sig}, nil); code != http.StatusUnauthorized {
		t.Errorf("nonce for another device: %d", code)
	}
	// Expired challenge.
	e.call(t, nil, "/v1/device/challenge", map[string]string{"device_id": id}, &ch)
	e.clock.Advance(ChallengeTTL)
	sig = b64.EncodeToString(ed25519.Sign(priv, AuthMessage(id, ch.Nonce)))
	if code, _ := e.call(t, nil, "/v1/device/token", map[string]string{"device_id": id, "nonce": ch.Nonce, "signature": sig}, nil); code != http.StatusUnauthorized {
		t.Errorf("expired challenge: %d", code)
	}
}

func TestDevicesAreScopedToTheirOwner(t *testing.T) {
	e := newEnv(t)
	ana, bo := e.user(t, "ana@x.com"), e.user(t, "bo@x.com")
	_, anaDev := e.enroll(t, ana)
	_, boDev := e.enroll(t, bo)
	e.pres.online[anaDev] = true

	list := func(c *http.Client) []DeviceView {
		resp, err := c.Get(e.srv.URL + "/v1/devices")
		if err != nil {
			t.Fatal(err)
		}
		defer resp.Body.Close()
		var out struct{ Devices []DeviceView }
		_ = json.NewDecoder(resp.Body).Decode(&out)
		return out.Devices
	}
	if l := list(ana); len(l) != 1 || l[0].ID != anaDev || !l[0].Online {
		t.Fatalf("ana sees %+v", l)
	}
	if l := list(bo); len(l) != 1 || l[0].ID != boDev || l[0].Online {
		t.Fatalf("bo sees %+v", l)
	}
	if resp, _ := http.Get(e.srv.URL + "/v1/devices"); resp.StatusCode != http.StatusUnauthorized {
		t.Fatalf("anonymous list = %d", resp.StatusCode)
	}

	// A code approved by Ana cannot then be claimed by Bo.
	pub, _, _ := ed25519.GenerateKey(nil)
	s := e.startPairing(t, pub)
	e.call(t, ana, "/v1/pair/approve", map[string]string{"code": s.UserCode}, nil)
	if code, _ := e.call(t, bo, "/v1/pair/approve", map[string]string{"code": s.UserCode}, nil); code != http.StatusNotFound {
		t.Fatalf("second approve = %d", code)
	}
}

func TestPairingRequiresSignedInUserAndOrigin(t *testing.T) {
	e := newEnv(t)
	pub, _, _ := ed25519.GenerateKey(nil)
	s := e.startPairing(t, pub)
	if code, _ := e.call(t, nil, "/v1/pair/approve", map[string]string{"code": s.UserCode}, nil); code != http.StatusUnauthorized {
		t.Fatalf("anonymous approve = %d", code)
	}
	ana := e.user(t, "ana@x.com")
	b, _ := json.Marshal(map[string]string{"code": s.UserCode})
	req, _ := http.NewRequest(http.MethodPost, e.srv.URL+"/v1/pair/approve", bytes.NewReader(b))
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Origin", "https://evil.example")
	resp, _ := ana.Do(req)
	resp.Body.Close()
	if resp.StatusCode != http.StatusForbidden {
		t.Fatalf("cross-origin approve = %d", resp.StatusCode)
	}
}

func TestCodeGuessingIsRateLimited(t *testing.T) {
	e := newEnv(t)
	ana := e.user(t, "ana@x.com")
	var last int
	for range 6 {
		last, _ = e.call(t, ana, "/v1/pair/lookup", map[string]string{"code": "BBBB-BBBB"}, nil)
	}
	if last != http.StatusTooManyRequests {
		t.Fatalf("6th guess = %d, want 429", last)
	}
}

func TestPairingExpires(t *testing.T) {
	e := newEnv(t)
	ana := e.user(t, "ana@x.com")
	pub, _, _ := ed25519.GenerateKey(nil)
	s := e.startPairing(t, pub)
	e.clock.Advance(PairingTTL)
	if code, _ := e.call(t, ana, "/v1/pair/approve", map[string]string{"code": s.UserCode}, nil); code != http.StatusNotFound {
		t.Fatalf("approve after expiry = %d", code)
	}
	if code, _ := e.call(t, nil, "/v1/pair/poll", map[string]string{"pairing_id": s.PairingID}, nil); code != http.StatusNotFound {
		t.Fatalf("poll after expiry = %d", code)
	}
}

func TestPairStartValidation(t *testing.T) {
	e := newEnv(t)
	pub, _, _ := ed25519.GenerateKey(nil)
	good := b64.EncodeToString(pub)
	cases := map[string]map[string]string{
		"short key":    {"public_key": "AAAA", "name": "n", "os": "macos"},
		"empty name":   {"public_key": good, "name": " ", "os": "macos"},
		"control char": {"public_key": good, "name": "a\x07b", "os": "macos"},
		"bad os":       {"public_key": good, "name": "n", "os": "amiga"},
	}
	for name, body := range cases {
		if code, _ := e.call(t, nil, "/v1/pair/start", body, nil); code != http.StatusBadRequest {
			t.Errorf("%s: %d", name, code)
		}
	}
}

func TestAuthMessageMatchesSharedFixture(t *testing.T) {
	raw, err := os.ReadFile("../../../proto/device-auth.json")
	if err != nil {
		t.Fatal(err)
	}
	var v struct {
		SeedHex   string `json:"seed_hex"`
		DeviceID  string `json:"device_id"`
		Nonce     string `json:"nonce"`
		Message   string `json:"message"`
		PublicKey string `json:"public_key"`
		Signature string `json:"signature"`
	}
	if err := json.Unmarshal(raw, &v); err != nil {
		t.Fatal(err)
	}
	if got := string(AuthMessage(v.DeviceID, v.Nonce)); got != v.Message {
		t.Fatalf("AuthMessage = %q, fixture %q", got, v.Message)
	}
	seed, _ := hex.DecodeString(v.SeedHex)
	key := ed25519.NewKeyFromSeed(seed)
	if b64.EncodeToString(key.Public().(ed25519.PublicKey)) != v.PublicKey {
		t.Fatal("public key mismatch")
	}
	if b64.EncodeToString(ed25519.Sign(key, []byte(v.Message))) != v.Signature {
		t.Fatal("signature mismatch")
	}
}

func TestRevokeEndpointIsOwnerOnlyAndAudited(t *testing.T) {
	e := newEnv(t)
	e.svc.Audit = &audit.Log{Store: e.svc.Store, Log: e.svc.Log, Now: e.clock.Now}
	var revoked []string
	e.svc.OnRevoke = func(id string) { revoked = append(revoked, id) }
	ana, bo := e.user(t, "ana@x.com"), e.user(t, "bo@x.com")
	key, anaDev := e.enroll(t, ana)

	if code, _ := e.call(t, bo, "/v1/devices/"+anaDev+"/revoke", map[string]string{}, nil); code != http.StatusNotFound {
		t.Fatalf("bo revoking ana's device = %d", code)
	}
	if len(revoked) != 0 {
		t.Fatal("hub notified for a refused revoke")
	}
	if code, body := e.call(t, ana, "/v1/devices/"+anaDev+"/revoke", map[string]string{}, nil); code != http.StatusNoContent {
		t.Fatalf("revoke = %d %s", code, body)
	}
	if len(revoked) != 1 || revoked[0] != anaDev {
		t.Fatalf("hub not told to disconnect: %v", revoked)
	}
	if code, _ := e.token(t, key, anaDev); code != http.StatusUnauthorized {
		t.Fatalf("revoked device can still authenticate: %d", code)
	}

	resp, err := ana.Get(e.srv.URL + "/v1/audit")
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	var out struct{ Events []store.AuditEvent }
	_ = json.NewDecoder(resp.Body).Decode(&out)
	actions := []string{}
	for _, ev := range out.Events {
		actions = append(actions, ev.Action)
	}
	if len(actions) < 2 || actions[0] != store.AuditDeviceRevoked || !slices.Contains(actions, store.AuditDevicePaired) {
		t.Fatalf("ana's audit = %v", actions)
	}
	resp2, err := bo.Get(e.srv.URL + "/v1/audit")
	if err != nil {
		t.Fatal(err)
	}
	defer resp2.Body.Close()
	var boOut struct{ Events []store.AuditEvent }
	_ = json.NewDecoder(resp2.Body).Decode(&boOut)
	for _, ev := range boOut.Events {
		if ev.DeviceID == anaDev {
			t.Fatalf("bo can see ana's events: %+v", ev)
		}
	}
}

func TestDeviceListIncludesPublicKey(t *testing.T) {
	e := newEnv(t)
	ana := e.user(t, "ana@x.com")
	key, id := e.enroll(t, ana)
	resp, err := ana.Get(e.srv.URL + "/v1/devices")
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	var out struct{ Devices []DeviceView }
	_ = json.NewDecoder(resp.Body).Decode(&out)
	want := b64.EncodeToString(key.Public().(ed25519.PublicKey))
	if len(out.Devices) != 1 || out.Devices[0].ID != id || out.Devices[0].PublicKey != want {
		t.Fatalf("devices = %+v", out.Devices)
	}
}

func TestBrowserSignInPairing(t *testing.T) {
	e := newEnv(t)
	ana := e.user(t, "ana@x.com")
	pub, priv, _ := ed25519.GenerateKey(nil)
	s := e.startPairing(t, pub)

	// The grant endpoint needs a signed-in browser.
	if code, _ := e.call(t, nil, "/v1/pair/grant", map[string]string{"pairing_id": s.PairingID}, nil); code != http.StatusUnauthorized {
		t.Fatalf("grant without sign-in = %d", code)
	}
	var g struct{ Grant, Name, OS string }
	if code, body := e.call(t, ana, "/v1/pair/grant", map[string]string{"pairing_id": s.PairingID}, &g); code != http.StatusOK || g.Grant == "" || g.Name != "Ana's MacBook" {
		t.Fatalf("grant = %d %s", code, body)
	}

	// Issuing a grant approves nothing by itself.
	var poll struct {
		Status   string `json:"status"`
		DeviceID string `json:"device_id"`
	}
	e.call(t, nil, "/v1/pair/poll", map[string]string{"pairing_id": s.PairingID}, &poll)
	if poll.Status != "pending" {
		t.Fatalf("poll after grant = %+v", poll)
	}

	// A grant only completes its own pairing, and a wrong attempt burns it.
	other := e.startPairing(t, func() ed25519.PublicKey { p, _, _ := ed25519.GenerateKey(nil); return p }())
	if code, _ := e.call(t, nil, "/v1/pair/complete", map[string]string{"pairing_id": other.PairingID, "grant": g.Grant}, nil); code != http.StatusForbidden {
		t.Fatalf("complete other pairing = %d", code)
	}
	if code, _ := e.call(t, nil, "/v1/pair/complete", map[string]string{"pairing_id": s.PairingID, "grant": g.Grant}, nil); code != http.StatusForbidden {
		t.Fatalf("reuse burned grant = %d", code)
	}

	e.call(t, ana, "/v1/pair/grant", map[string]string{"pairing_id": s.PairingID}, &g)
	var done struct{ Status, DeviceID string }
	if code, body := e.call(t, nil, "/v1/pair/complete", map[string]string{"pairing_id": s.PairingID, "grant": g.Grant}, &done); code != http.StatusOK || done.Status != "approved" {
		t.Fatalf("complete = %d %s", code, body)
	}
	e.call(t, nil, "/v1/pair/poll", map[string]string{"pairing_id": s.PairingID}, &poll)
	if poll.Status != "approved" || poll.DeviceID == "" {
		t.Fatalf("poll after complete = %+v", poll)
	}
	if code, _ := e.token(t, priv, poll.DeviceID); code != http.StatusOK {
		t.Fatalf("device token after browser pairing = %d", code)
	}
	// Single use, and the pairing is no longer pending.
	if code, _ := e.call(t, nil, "/v1/pair/complete", map[string]string{"pairing_id": s.PairingID, "grant": g.Grant}, nil); code != http.StatusForbidden {
		t.Fatalf("grant reuse = %d", code)
	}
	if code, _ := e.call(t, ana, "/v1/pair/grant", map[string]string{"pairing_id": s.PairingID}, nil); code != http.StatusNotFound {
		t.Fatalf("grant for completed pairing = %d", code)
	}

	// Grants expire.
	s2 := e.startPairing(t, func() ed25519.PublicKey { p, _, _ := ed25519.GenerateKey(nil); return p }())
	e.call(t, ana, "/v1/pair/grant", map[string]string{"pairing_id": s2.PairingID}, &g)
	e.clock.Advance(GrantTTL + time.Second)
	if code, _ := e.call(t, nil, "/v1/pair/complete", map[string]string{"pairing_id": s2.PairingID, "grant": g.Grant}, nil); code != http.StatusForbidden {
		t.Fatalf("expired grant = %d", code)
	}
}

func TestUnpaidAccountsCannotAddComputers(t *testing.T) {
	e := newEnv(t)
	e.svc.Access = func(context.Context, string) (bool, error) { return false, nil }
	ana := e.user(t, "ana@x.com")
	pub, _, _ := ed25519.GenerateKey(nil)
	s := e.startPairing(t, pub)
	if code, _ := e.call(t, ana, "/v1/pair/grant", map[string]string{"pairing_id": s.PairingID}, nil); code != http.StatusPaymentRequired {
		t.Fatalf("grant without paying = %d", code)
	}
	if code, _ := e.call(t, ana, "/v1/pair/approve", map[string]string{"code": s.UserCode}, nil); code != http.StatusPaymentRequired {
		t.Fatalf("approve without paying = %d", code)
	}
}
