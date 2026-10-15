// Package auth handles user sign-in (OIDC / OAuth2 / dev), browser sessions, and
// the origin checks that protect cookie-authenticated endpoints from CSRF.
package auth

import (
	"context"
	"errors"
	"log/slog"
	"net/http"
	"net/mail"
	"net/url"
	"slices"
	"strings"
	"time"

	"golang.org/x/oauth2"

	"carson/server/internal/httpx"
	"carson/server/internal/ratelimit"
	"carson/server/internal/store"
)

const (
	CookieName      = "carson_session"
	SessionTTL      = 30 * 24 * time.Hour
	loginStateTTL   = 10 * time.Minute
	providerTimeout = 15 * time.Second
)

// Service implements sign-in and session lookup.
type Service struct {
	Store     *store.Store
	Providers map[string]Provider
	PublicURL *url.URL
	// AllowedOrigins are extra browser origins (besides PublicURL) for cookie requests.
	AllowedOrigins []string
	Dev            bool
	TrustProxy     bool
	Log            *slog.Logger
	Now            func() time.Time
	LoginLimiter   *ratelimit.Limiter
}

type ctxKey struct{}

// UserFrom returns the signed-in user stored in the request context by RequireUser.
func UserFrom(ctx context.Context) (store.User, bool) {
	u, ok := ctx.Value(ctxKey{}).(store.User)
	return u, ok
}

// Routes registers the /auth/* endpoints and /v1/me.
func (s *Service) Routes(mux *http.ServeMux) {
	mux.HandleFunc("GET /auth/providers", s.handleProviders)
	mux.HandleFunc("GET /auth/{provider}/login", s.handleLogin)
	mux.HandleFunc("GET /auth/{provider}/callback", s.handleCallback)
	mux.HandleFunc("POST /auth/dev/login", s.handleDevLogin)
	mux.HandleFunc("POST /auth/logout", s.handleLogout)
	mux.Handle("GET /v1/me", s.RequireUser(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		u, _ := UserFrom(r.Context())
		httpx.WriteJSON(w, http.StatusOK, u)
	})))
}

// SessionUser resolves the session cookie, if any.
func (s *Service) SessionUser(r *http.Request) (store.User, error) {
	c, err := r.Cookie(CookieName)
	if err != nil || c.Value == "" {
		return store.User{}, store.ErrNotFound
	}
	return s.Store.UserForWebSession(r.Context(), store.HashToken(c.Value), s.Now())
}

// RequireUser rejects requests without a valid session and, for state-changing
// methods, requests whose Origin is not ours (CSRF defense on top of SameSite=Lax).
func (s *Service) RequireUser(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodGet && r.Method != http.MethodHead && !s.OriginAllowed(r) {
			httpx.WriteError(w, http.StatusForbidden, "bad_origin", "request origin not allowed")
			return
		}
		u, err := s.SessionUser(r)
		if errors.Is(err, store.ErrNotFound) {
			httpx.WriteError(w, http.StatusUnauthorized, "unauthenticated", "sign in required")
			return
		}
		if err != nil {
			s.Log.Error("session lookup", "err", err)
			httpx.WriteError(w, http.StatusInternalServerError, "internal", "internal error")
			return
		}
		next.ServeHTTP(w, r.WithContext(context.WithValue(r.Context(), ctxKey{}, u)))
	})
}

// OriginAllowed reports whether the request's Origin header is the public URL or an allowed origin.
// A missing Origin is rejected: browsers always send it on POST and WebSocket requests.
func (s *Service) OriginAllowed(r *http.Request) bool {
	origin := r.Header.Get("Origin")
	if origin == "" {
		return false
	}
	return origin == s.PublicURL.Scheme+"://"+s.PublicURL.Host || slices.Contains(s.AllowedOrigins, origin)
}

func (s *Service) handleProviders(w http.ResponseWriter, _ *http.Request) {
	names := make([]string, 0, len(s.Providers))
	for name := range s.Providers {
		names = append(names, name)
	}
	slices.Sort(names)
	httpx.WriteJSON(w, http.StatusOK, map[string]any{"providers": names, "dev": s.Dev})
}

func (s *Service) redirectURL(provider string) string {
	return s.PublicURL.JoinPath("auth", provider, "callback").String()
}

func (s *Service) handleLogin(w http.ResponseWriter, r *http.Request) {
	p, ok := s.Providers[r.PathValue("provider")]
	if !ok {
		httpx.WriteError(w, http.StatusNotFound, "unknown_provider", "unknown login provider")
		return
	}
	if !s.LoginLimiter.Allow(httpx.ClientIP(r, s.TrustProxy)) {
		httpx.WriteError(w, http.StatusTooManyRequests, "rate_limited", "too many login attempts")
		return
	}
	state, _ := store.NewToken()
	nonce, _ := store.NewToken()
	verifier := oauth2.GenerateVerifier()
	st := store.OAuthState{
		State: state, Provider: p.Name(), Verifier: verifier, Nonce: nonce,
		ReturnTo: SafeReturnTo(r.URL.Query().Get("return_to")),
		Expires:  s.Now().Add(loginStateTTL),
	}
	if err := s.Store.PutOAuthState(r.Context(), st); err != nil {
		s.Log.Error("store oauth state", "err", err)
		httpx.WriteError(w, http.StatusInternalServerError, "internal", "internal error")
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), providerTimeout)
	defer cancel()
	target, err := p.AuthURL(ctx, state, verifier, nonce, s.redirectURL(p.Name()))
	if err != nil {
		s.Log.Error("build auth url", "provider", p.Name(), "err", err)
		httpx.WriteError(w, http.StatusBadGateway, "provider_unavailable", "login provider unavailable")
		return
	}
	http.Redirect(w, r, target, http.StatusFound)
}

func (s *Service) handleCallback(w http.ResponseWriter, r *http.Request) {
	fail := func(code string) {
		http.Redirect(w, r, "/#/login?error="+url.QueryEscape(code), http.StatusFound)
	}
	q := r.URL.Query()
	name := r.PathValue("provider")
	p, ok := s.Providers[name]
	if !ok {
		fail("unknown_provider")
		return
	}
	st, err := s.Store.TakeOAuthState(r.Context(), q.Get("state"), s.Now())
	if err != nil || st.Provider != name {
		fail("invalid_state")
		return
	}
	if q.Get("error") != "" || q.Get("code") == "" {
		fail("access_denied")
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), providerTimeout)
	defer cancel()
	ident, err := p.Exchange(ctx, q.Get("code"), st.Verifier, st.Nonce, s.redirectURL(name))
	if errors.Is(err, ErrUnverifiedEmail) {
		fail("unverified_email")
		return
	}
	if err != nil {
		s.Log.Warn("login exchange failed", "provider", name, "err", err)
		fail("login_failed")
		return
	}
	if err := s.startSession(w, r, ident); err != nil {
		fail("internal")
		return
	}
	http.Redirect(w, r, st.ReturnTo, http.StatusFound)
}

// handleDevLogin signs in by email without verification. Only exists with CARSON_DEV=true.
func (s *Service) handleDevLogin(w http.ResponseWriter, r *http.Request) {
	if !s.Dev {
		http.NotFound(w, r)
		return
	}
	if !s.OriginAllowed(r) {
		httpx.WriteError(w, http.StatusForbidden, "bad_origin", "request origin not allowed")
		return
	}
	var body struct {
		Email string `json:"email"`
	}
	if err := httpx.DecodeJSON(w, r, &body); err != nil {
		httpx.WriteError(w, http.StatusBadRequest, "bad_request", err.Error())
		return
	}
	addr, err := mail.ParseAddress(body.Email)
	if err != nil || addr.Name != "" {
		httpx.WriteError(w, http.StatusBadRequest, "bad_email", "invalid email address")
		return
	}
	email := strings.ToLower(addr.Address)
	if err := s.startSession(w, r, store.Identity{Provider: "dev", Subject: email, Email: email, Name: strings.Split(email, "@")[0]}); err != nil {
		httpx.WriteError(w, http.StatusInternalServerError, "internal", "internal error")
		return
	}
	httpx.WriteJSON(w, http.StatusOK, map[string]string{"status": "ok"})
}

func (s *Service) startSession(w http.ResponseWriter, r *http.Request, ident store.Identity) error {
	now := s.Now()
	u, err := s.Store.UpsertUserForIdentity(r.Context(), ident, now)
	if err != nil {
		s.Log.Error("upsert user", "provider", ident.Provider, "err", err)
		return err
	}
	token, hash := store.NewToken()
	expires := now.Add(SessionTTL)
	if err := s.Store.CreateWebSession(r.Context(), hash, u.ID, now, expires, httpx.ClientIP(r, s.TrustProxy), r.UserAgent()); err != nil {
		s.Log.Error("create session", "err", err)
		return err
	}
	http.SetCookie(w, &http.Cookie{
		Name: CookieName, Value: token, Path: "/", Expires: expires,
		HttpOnly: true, Secure: s.PublicURL.Scheme == "https", SameSite: http.SameSiteLaxMode,
	})
	s.Log.Info("signed in", "user_id", u.ID, "provider", ident.Provider)
	return nil
}

func (s *Service) handleLogout(w http.ResponseWriter, r *http.Request) {
	if !s.OriginAllowed(r) {
		httpx.WriteError(w, http.StatusForbidden, "bad_origin", "request origin not allowed")
		return
	}
	if c, err := r.Cookie(CookieName); err == nil && c.Value != "" {
		if err := s.Store.DeleteWebSession(r.Context(), store.HashToken(c.Value)); err != nil {
			s.Log.Error("delete session", "err", err)
		}
	}
	http.SetCookie(w, &http.Cookie{
		Name: CookieName, Value: "", Path: "/", MaxAge: -1,
		HttpOnly: true, Secure: s.PublicURL.Scheme == "https", SameSite: http.SameSiteLaxMode,
	})
	w.WriteHeader(http.StatusNoContent)
}

// SafeReturnTo allows only same-site relative paths, preventing open redirects.
func SafeReturnTo(v string) string {
	if v == "" || !strings.HasPrefix(v, "/") || strings.HasPrefix(v, "//") || strings.ContainsAny(v, "\\\r\n") {
		return "/"
	}
	return v
}
