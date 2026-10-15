// Package httpapi wires carson-server's HTTP routes and middleware.
package httpapi

import (
	"bufio"
	"context"
	"log/slog"
	"net"
	"net/http"
	"time"

	"carson/server/internal/auth"
	"carson/server/internal/config"
	"carson/server/internal/devices"
	"carson/server/internal/httpx"
	"carson/server/internal/ratelimit"
	"carson/server/internal/signaling"
	"carson/server/internal/store"
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

	authSvc := &auth.Service{
		Store: st, Providers: providers, PublicURL: cfg.PublicURL, AllowedOrigins: cfg.AllowedOrigins,
		Dev: cfg.Dev, TrustProxy: cfg.TrustProxy, Log: log, Now: now,
		LoginLimiter: ratelimit.New(20, time.Minute, 20),
	}
	hub := signaling.NewHub()
	devSvc := &devices.Service{
		Store: st, Auth: authSvc, Presence: hub, TrustProxy: cfg.TrustProxy, Log: log, Now: now,
		Limits: devices.Limiters{
			PairStart:  ratelimit.New(10, 10*time.Minute, 10),
			PairCode:   ratelimit.New(10, time.Minute, 10),
			PairPoll:   ratelimit.New(60, time.Minute, 30),
			DeviceAuth: ratelimit.New(30, time.Minute, 30),
		},
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
	mux.Handle("GET /v1/ws", &signaling.Handler{
		Hub: hub, Store: st, Auth: authSvc, Devices: devSvc, Log: log, Now: now,
		PingInterval: 15 * time.Second, PingTimeout: 30 * time.Second,
		STUNURLs: cfg.STUNURLs, TrustProxy: cfg.TrustProxy,
		SessionLimiter: ratelimit.New(10, time.Minute, 5),
	})
	if cfg.WebDir != "" {
		mux.Handle("GET /", http.FileServer(http.Dir(cfg.WebDir)))
	}

	return &Server{Handler: logRequests(log, securityHeaders(mux)), Hub: hub}
}

func securityHeaders(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		h := w.Header()
		h.Set("X-Content-Type-Options", "nosniff")
		h.Set("X-Frame-Options", "DENY")
		h.Set("Referrer-Policy", "no-referrer")
		h.Set("Content-Security-Policy", "default-src 'self'; connect-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'self'; frame-ancestors 'none'")
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
