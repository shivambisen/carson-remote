// Package httpapi wires carson-server's HTTP routes and middleware.
package httpapi

import (
	"bufio"
	"context"
	"log/slog"
	"net"
	"net/http"
	"strings"
	"time"

	"carson/server/internal/audit"
	"carson/server/internal/auth"
	"carson/server/internal/billing"
	"carson/server/internal/config"
	"carson/server/internal/devices"
	"carson/server/internal/httpx"
	"carson/server/internal/ratelimit"
	"carson/server/internal/signaling"
	"carson/server/internal/store"
	"carson/server/internal/turn"
)

// Version is overridden at build time via -ldflags "-X carson/server/internal/httpapi.Version=...".
var Version = "dev"

// Server bundles the services behind the HTTP API.
type Server struct {
	Handler http.Handler
	Hub     *signaling.Hub
}

// New builds the full route table from config and an opened store.
func New(cfg config.Config, st *store.Store, log *slog.Logger) *Server {
	now := time.Now
	providers := map[string]auth.Provider{}
	if cfg.Google.Enabled() {
		providers["google"] = auth.NewOIDC("google", "https://accounts.google.com", cfg.Google.ClientID, cfg.Google.ClientSecret)
	}
	if cfg.GitHub.Enabled() {
		providers["github"] = auth.NewGitHub(cfg.GitHub.ClientID, cfg.GitHub.ClientSecret)
	}

	auditLog := &audit.Log{Store: st, Log: log, Now: now}
	authSvc := &auth.Service{
		Store: st, Providers: providers, PublicURL: cfg.PublicURL, AltPublicURLs: cfg.AltPublicURLs, AllowedOrigins: cfg.AllowedOrigins,
		Dev: cfg.Dev, TrustProxy: cfg.TrustProxy, Log: log, Now: now,
		LoginLimiter: ratelimit.New(20, time.Minute, 20),
		Audit:        auditLog,
	}
	hub := signaling.NewHub()
	devSvc := &devices.Service{
		Store: st, Auth: authSvc, Presence: hub, TrustProxy: cfg.TrustProxy, Log: log, Now: now,
		Audit: auditLog,
		Limits: devices.Limiters{
			Revoke:     ratelimit.New(10, time.Minute, 10),
			PairStart:  ratelimit.New(10, 10*time.Minute, 10),
			PairCode:   ratelimit.New(10, time.Minute, 10),
			PairPoll:   ratelimit.New(60, time.Minute, 30),
			DeviceAuth: ratelimit.New(30, time.Minute, 30),
		},
	}

	billingSvc := &billing.Service{
		Store: st, Auth: authSvc, KeyID: cfg.RazorpayKeyID, KeySecret: cfg.RazorpayKeySecret,
		WebhookSecret: cfg.RazorpayWebhookSecret, Amount: cfg.PricePaise, Currency: "INR",
		Log: log, Now: now, Audit: auditLog, OrderLimiter: ratelimit.New(10, time.Minute, 5),
	}
	if billingSvc.Enabled() {
		devSvc.Access = billingSvc.HasAccess
		log.Info("payments enabled", "price_paise", cfg.PricePaise)
	}

	mux := http.NewServeMux()
	mux.HandleFunc("GET /healthz", func(w http.ResponseWriter, _ *http.Request) {
		httpx.WriteJSON(w, http.StatusOK, map[string]string{"status": "ok"})
	})
	mux.HandleFunc("GET /readyz", func(w http.ResponseWriter, r *http.Request) {
		ctx, cancel := context.WithTimeout(r.Context(), 2*time.Second)
		defer cancel()
		if err := st.Ping(ctx); err != nil {
			httpx.WriteError(w, http.StatusServiceUnavailable, "db_unavailable", "database unavailable")
			return
		}
		httpx.WriteJSON(w, http.StatusOK, map[string]string{"status": "ready"})
	})
	mux.HandleFunc("GET /v1/version", func(w http.ResponseWriter, _ *http.Request) {
		httpx.WriteJSON(w, http.StatusOK, map[string]string{"name": "carson-server", "version": Version})
	})
	authSvc.Routes(mux)
	devSvc.Routes(mux)
	billingSvc.Routes(mux)
	ws := &signaling.Handler{
		Hub: hub, Store: st, Auth: authSvc, Devices: devSvc, Log: log, Now: now,
		PingInterval: 15 * time.Second, PingTimeout: 30 * time.Second,
		ICE: &turn.Issuer{
			Secret: cfg.TURNSecret, TURNURLs: cfg.TURNURLs, STUNURLs: cfg.STUNURLs, TTL: cfg.TURNTTL,
			Cloudflare:       &turn.Cloudflare{KeyID: cfg.CloudflareTURNKeyID, APIToken: cfg.CloudflareTURNToken},
			StaticTURNURLs:   cfg.TURNStaticURLs,
			StaticUsername:   cfg.TURNStaticUsername,
			StaticCredential: cfg.TURNStaticCredential,
			OnError:          func(err error) { log.Error("relay credentials", "err", err) },
		},
		TrustProxy:     cfg.TrustProxy,
		SessionLimiter: ratelimit.New(10, time.Minute, 5),
		UpgradeLimiter: ratelimit.New(30, time.Minute, 30),
		ResumeLimiter:  ratelimit.New(10, time.Minute, 10),
		Audit:          auditLog,
	}
	devSvc.OnRevoke = ws.RevokeDevice
	if billingSvc.Enabled() {
		ws.Access = billingSvc.HasAccess
	}
	mux.Handle("GET /v1/ws", ws)
	if cfg.WebDir != "" {
		mux.Handle("GET /", webHandler(cfg.WebDir))
	}

	// Coarse per-IP guard in front of everything; endpoint limits above are much tighter.
	global := ratelimit.New(1200, time.Minute, 300)
	limited := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if !global.Allow(httpx.ClientIP(r, cfg.TrustProxy)) {
			httpx.WriteError(w, http.StatusTooManyRequests, "rate_limited", "too many requests")
			return
		}
		mux.ServeHTTP(w, r)
	})
	return &Server{Handler: logRequests(log, securityHeaders(limited)), Hub: hub}
}

func securityHeaders(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		h := w.Header()
		h.Set("X-Content-Type-Options", "nosniff")
		h.Set("X-Frame-Options", "DENY")
		h.Set("Referrer-Policy", "no-referrer")
		// Google Fonts for the typefaces; Razorpay Checkout (script, its frame and API calls).
		h.Set("Content-Security-Policy", "default-src 'self'; "+
			"script-src 'self' https://*.razorpay.com; "+
			"style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; "+
			"font-src 'self' https://fonts.gstatic.com; "+
			"connect-src 'self' https://*.razorpay.com; "+
			"frame-src https://*.razorpay.com; "+
			"img-src 'self' data: https://*.razorpay.com; base-uri 'none'; form-action 'self'; frame-ancestors 'none'")
		next.ServeHTTP(w, r)
	})
}

type statusRecorder struct {
	http.ResponseWriter
	status int
}

func (s *statusRecorder) WriteHeader(code int) {
	s.status = code
	s.ResponseWriter.WriteHeader(code)
}

// Unwrap lets http.ResponseController reach the underlying writer.
func (s *statusRecorder) Unwrap() http.ResponseWriter { return s.ResponseWriter }

// Hijack supports the WebSocket upgrade through this wrapper.
func (s *statusRecorder) Hijack() (net.Conn, *bufio.ReadWriter, error) {
	s.status = http.StatusSwitchingProtocols
	return http.NewResponseController(s.ResponseWriter).Hijack()
}

func logRequests(log *slog.Logger, next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		start := time.Now()
		rec := &statusRecorder{ResponseWriter: w, status: http.StatusOK}
		next.ServeHTTP(rec, r)
		log.LogAttrs(r.Context(), slog.LevelInfo, "http",
			slog.String("method", r.Method),
			slog.String("path", r.URL.Path),
			slog.Int("status", rec.status),
			slog.Duration("dur", time.Since(start)),
		)
	})
}

// webHandler serves the built viewer. Hashed assets (assets/index-abc123.js) never change, so
// they are cached for a year; everything else (index.html) is revalidated on every load so a
// deploy is picked up immediately.
func webHandler(dir string) http.Handler {
	files := http.FileServer(http.Dir(dir))
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if strings.HasPrefix(r.URL.Path, "/assets/") {
			w.Header().Set("Cache-Control", "public, max-age=31536000, immutable")
		} else {
			w.Header().Set("Cache-Control", "no-cache")
		}
		files.ServeHTTP(w, r)
	})
}
