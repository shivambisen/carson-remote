package auth

import (
	"crypto/rand"
	"crypto/rsa"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"net/http/cookiejar"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
	"time"

	jose "github.com/go-jose/go-jose/v4"
	"github.com/go-jose/go-jose/v4/jwt"

	"carson/server/internal/ratelimit"
	"carson/server/internal/store"
	"carson/server/internal/store/storetest"
)

type harness struct {
	svc    *Service
	srv    *httptest.Server
	client *http.Client
}

func newHarness(t *testing.T, dev bool, providers ...Provider) *harness {
	t.Helper()
	st := storetest.Open(t)
	h := &harness{}
	mux := http.NewServeMux()
	h.srv = httptest.NewServer(mux)
	t.Cleanup(h.srv.Close)
	pub, _ := url.Parse(h.srv.URL)
	h.svc = &Service{
		Store: st, Providers: map[string]Provider{}, PublicURL: pub, Dev: dev,
		Log: slog.New(slog.NewTextHandler(io.Discard, nil)), Now: time.Now,
		LoginLimiter: ratelimit.New(100, time.Minute, 100),
	}
	for _, p := range providers {
		h.svc.Providers[p.Name()] = p
	}
	h.svc.Routes(mux)
	jar, _ := cookiejar.New(nil)
	h.client = &http.Client{Jar: jar, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	return h
}

func (h *harness) post(t *testing.T, path, origin, body string) *http.Response {
	t.Helper()
	req, _ := http.NewRequest(http.MethodPost, h.srv.URL+path, strings.NewReader(body))
	req.Header.Set("Content-Type", "application/json")
	if origin != "" {
		req.Header.Set("Origin", origin)
	}
	resp, err := h.client.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	resp.Body.Close()
	return resp
}

func (h *harness) get(t *testing.T, path string) *http.Response {
	t.Helper()
	resp, err := h.client.Get(h.srv.URL + path)
	if err != nil {
		t.Fatal(err)
	}
	return resp
}

func (h *harness) me(t *testing.T) (store.User, int) {
	t.Helper()
	resp := h.get(t, "/v1/me")
	defer resp.Body.Close()
	var u store.User
	_ = json.NewDecoder(resp.Body).Decode(&u)
	return u, resp.StatusCode
}

func TestDevLoginSessionLifecycle(t *testing.T) {
	h := newHarness(t, true)
	if _, code := h.me(t); code != http.StatusUnauthorized {
		t.Fatalf("me before login = %d", code)
	}
	if resp := h.post(t, "/auth/dev/login", h.srv.URL, `{"email":"Ana@Example.com"}`); resp.StatusCode != http.StatusOK {
		t.Fatalf("dev login = %d", resp.StatusCode)
	}
	u, code := h.me(t)
	if code != http.StatusOK || u.Email != "ana@example.com" {
		t.Fatalf("me after login = %d %+v", code, u)
	}
	if resp := h.post(t, "/auth/logout", h.srv.URL, ``); resp.StatusCode != http.StatusNoContent {
		t.Fatalf("logout = %d", resp.StatusCode)
	}
	if _, code := h.me(t); code != http.StatusUnauthorized {
		t.Fatalf("me after logout = %d", code)
	}
}

func TestDevLoginDisabledOutsideDevMode(t *testing.T) {
	h := newHarness(t, false)
	if resp := h.post(t, "/auth/dev/login", h.srv.URL, `{"email":"a@b.c"}`); resp.StatusCode != http.StatusNotFound {
		t.Fatalf("dev login in prod = %d, want 404", resp.StatusCode)
	}
}

func TestStateChangingRequestsRequireOrigin(t *testing.T) {
	h := newHarness(t, true)
	for _, origin := range []string{"", "https://evil.example"} {
		if resp := h.post(t, "/auth/dev/login", origin, `{"email":"a@b.c"}`); resp.StatusCode != http.StatusForbidden {
			t.Errorf("origin %q: status = %d, want 403", origin, resp.StatusCode)
		}
	}
	h.svc.AllowedOrigins = []string{"http://localhost:5173"}
	if resp := h.post(t, "/auth/dev/login", "http://localhost:5173", `{"email":"a@b.c"}`); resp.StatusCode != http.StatusOK {
		t.Errorf("allowed extra origin: status = %d", resp.StatusCode)
	}
}

func TestDevLoginRejectsBadEmail(t *testing.T) {
	h := newHarness(t, true)
	for _, body := range []string{`{"email":"nope"}`, `{"email":"Ana <a@b.c>"}`, `{}`} {
		if resp := h.post(t, "/auth/dev/login", h.srv.URL, body); resp.StatusCode != http.StatusBadRequest {
			t.Errorf("%s: status = %d", body, resp.StatusCode)
		}
	}
}

func TestSafeReturnTo(t *testing.T) {
	cases := map[string]string{
		"":                       "/",
		"/#/devices":             "/#/devices",
		"/pair?x=1":              "/pair?x=1",
		"https://evil.example/":  "/",
		"//evil.example":         "/",
		"/\\evil.example":        "/",
		"javascript:alert(1)":    "/",
		"/ok\r\nSet-Cookie: x=y": "/",
	}
	for in, want := range cases {
		if got := SafeReturnTo(in); got != want {
			t.Errorf("SafeReturnTo(%q) = %q, want %q", in, got, want)
		}
	}
}

// startLogin follows /auth/{p}/login and returns the provider authorize URL.
func startLogin(t *testing.T, h *harness, provider, returnTo string) *url.URL {
	t.Helper()
	resp := h.get(t, "/auth/"+provider+"/login?return_to="+url.QueryEscape(returnTo))
	resp.Body.Close()
	if resp.StatusCode != http.StatusFound {
		t.Fatalf("login start = %d", resp.StatusCode)
	}
	loc, err := url.Parse(resp.Header.Get("Location"))
	if err != nil {
		t.Fatal(err)
	}
	q := loc.Query()
	if q.Get("code_challenge_method") != "S256" || q.Get("code_challenge") == "" || q.Get("state") == "" {
		t.Fatalf("authorize URL missing PKCE/state: %s", loc)
	}
	return loc
}

func callback(t *testing.T, h *harness, provider, state, code string) string {
	t.Helper()
	resp := h.get(t, "/auth/"+provider+"/callback?state="+url.QueryEscape(state)+"&code="+url.QueryEscape(code))
	resp.Body.Close()
	if resp.StatusCode != http.StatusFound {
		t.Fatalf("callback = %d", resp.StatusCode)
	}
	return resp.Header.Get("Location")
}

func fakeGitHub(t *testing.T, emails string) *GitHubProvider {
	t.Helper()
	mux := http.NewServeMux()
	mux.HandleFunc("POST /token", func(w http.ResponseWriter, r *http.Request) {
		_ = r.ParseForm()
		if r.Form.Get("code") != "good-code" || r.Form.Get("code_verifier") == "" {
			http.Error(w, `{"error":"bad_verification_code"}`, http.StatusBadRequest)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(w, `{"access_token":"gho_x","token_type":"bearer"}`)
	})
	mux.HandleFunc("GET /user", func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("Authorization") != "Bearer gho_x" {
			w.WriteHeader(http.StatusUnauthorized)
			return
		}
		_, _ = io.WriteString(w, `{"id":4242,"login":"ana","name":""}`)
	})
	mux.HandleFunc("GET /user/emails", func(w http.ResponseWriter, _ *http.Request) {
		_, _ = io.WriteString(w, emails)
	})
	srv := httptest.NewServer(mux)
	t.Cleanup(srv.Close)
	p := NewGitHub("cid", "secret")
	p.AuthEndpoint, p.TokenEndpoint, p.APIBase, p.HTTPClient = srv.URL+"/authorize", srv.URL+"/token", srv.URL, srv.Client()
	return p
}

func TestGitHubLoginFlow(t *testing.T) {
	gh := fakeGitHub(t, `[{"email":"old@x.com","primary":false,"verified":true},{"email":"ana@x.com","primary":true,"verified":true}]`)
	h := newHarness(t, false, gh)

	auth := startLogin(t, h, "github", "/#/pair")
	state := auth.Query().Get("state")
	if got := callback(t, h, "github", state, "good-code"); got != "/#/pair" {
		t.Fatalf("redirect after login = %q", got)
	}
	u, code := h.me(t)
	if code != http.StatusOK || u.Email != "ana@x.com" || u.DisplayName != "ana" {
		t.Fatalf("me = %d %+v", code, u)
	}
	// The state is single-use: replaying the callback must fail.
	if got := callback(t, h, "github", state, "good-code"); !strings.Contains(got, "error=invalid_state") {
		t.Fatalf("replayed callback redirect = %q", got)
	}
}

func TestGitHubLoginRejectsUnverifiedEmail(t *testing.T) {
	gh := fakeGitHub(t, `[{"email":"ana@x.com","primary":true,"verified":false}]`)
	h := newHarness(t, false, gh)
	state := startLogin(t, h, "github", "/").Query().Get("state")
	if got := callback(t, h, "github", state, "good-code"); !strings.Contains(got, "error=unverified_email") {
		t.Fatalf("redirect = %q", got)
	}
	if _, code := h.me(t); code != http.StatusUnauthorized {
		t.Fatal("unverified login must not create a session")
	}
}

func TestCallbackRejectsForgedState(t *testing.T) {
	h := newHarness(t, false, fakeGitHub(t, `[]`))
	if got := callback(t, h, "github", "forged", "good-code"); !strings.Contains(got, "error=invalid_state") {
		t.Fatalf("redirect = %q", got)
	}
}

// fakeIssuer is a minimal OIDC provider: discovery, JWKS, and a token endpoint that
// returns an RS256 id_token for whatever nonce the test registers.
type fakeIssuer struct {
	srv   *httptest.Server
	key   *rsa.PrivateKey
	nonce string
	email string
}

func newFakeIssuer(t *testing.T) *fakeIssuer {
	t.Helper()
	key, err := rsa.GenerateKey(rand.Reader, 2048)
	if err != nil {
		t.Fatal(err)
	}
	f := &fakeIssuer{key: key, email: "ana@x.com"}
	mux := http.NewServeMux()
	f.srv = httptest.NewServer(mux)
	t.Cleanup(f.srv.Close)
	mux.HandleFunc("GET /.well-known/openid-configuration", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]any{
			"issuer": f.srv.URL, "authorization_endpoint": f.srv.URL + "/authorize",
			"token_endpoint": f.srv.URL + "/token", "jwks_uri": f.srv.URL + "/jwks",
			"id_token_signing_alg_values_supported": []string{"RS256"},
		})
	})
	mux.HandleFunc("GET /jwks", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(jose.JSONWebKeySet{Keys: []jose.JSONWebKey{{Key: &key.PublicKey, KeyID: "k1", Algorithm: "RS256", Use: "sig"}}})
	})
	mux.HandleFunc("POST /token", func(w http.ResponseWriter, _ *http.Request) {
		signer, _ := jose.NewSigner(jose.SigningKey{Algorithm: jose.RS256, Key: key}, (&jose.SignerOptions{}).WithHeader("kid", "k1"))
		now := time.Now()
		raw, _ := jwt.Signed(signer).Claims(map[string]any{
			"iss": f.srv.URL, "sub": "google-sub-1", "aud": "cid", "nonce": f.nonce,
			"iat": now.Unix(), "exp": now.Add(time.Minute).Unix(),
			"email": f.email, "email_verified": true, "name": "Ana",
		}).Serialize()
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(map[string]any{"access_token": "at", "token_type": "Bearer", "id_token": raw})
	})
	return f
}

func TestOIDCLoginFlow(t *testing.T) {
	iss := newFakeIssuer(t)
	h := newHarness(t, false, NewOIDC("google", iss.srv.URL, "cid", "secret"))

	auth := startLogin(t, h, "google", "/")
	iss.nonce = auth.Query().Get("nonce")
	if got := callback(t, h, "google", auth.Query().Get("state"), "c"); got != "/" {
		t.Fatalf("redirect = %q", got)
	}
	if u, code := h.me(t); code != http.StatusOK || u.Email != "ana@x.com" {
		t.Fatalf("me = %d %+v", code, u)
	}
}

func TestOIDCLoginRejectsNonceMismatch(t *testing.T) {
	iss := newFakeIssuer(t)
	h := newHarness(t, false, NewOIDC("google", iss.srv.URL, "cid", "secret"))
	auth := startLogin(t, h, "google", "/")
	iss.nonce = "attacker-nonce"
	if got := callback(t, h, "google", auth.Query().Get("state"), "c"); !strings.Contains(got, "error=login_failed") {
		t.Fatalf("redirect = %q", got)
	}
}

func TestAltPublicURLs(t *testing.T) {
	pub, _ := url.Parse("https://app.example.in")
	alt, _ := url.Parse("https://example.onrender.com")
	s := &Service{PublicURL: pub, AltPublicURLs: []*url.URL{alt}}
	req := func(host, origin string) *http.Request {
		r := httptest.NewRequest(http.MethodPost, "/", nil)
		r.Host = host
		if origin != "" {
			r.Header.Set("Origin", origin)
		}
		return r
	}
	if got := s.redirectURL(req("example.onrender.com", ""), "google"); got != "https://example.onrender.com/auth/google/callback" {
		t.Errorf("alt host callback = %q", got)
	}
	if got := s.redirectURL(req("app.example.in", ""), "google"); got != "https://app.example.in/auth/google/callback" {
		t.Errorf("main host callback = %q", got)
	}
	if got := s.PublicURLFor(req("evil.test", "")); got != pub {
		t.Errorf("unknown host = %v, want the main URL", got)
	}
	for origin, want := range map[string]bool{"https://app.example.in": true, "https://example.onrender.com": true, "https://evil.test": false} {
		if got := s.OriginAllowed(req("app.example.in", origin)); got != want {
			t.Errorf("OriginAllowed(%q) = %v, want %v", origin, got, want)
		}
	}
}
