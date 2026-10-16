// Package config loads carson-server settings from CARSON_* environment variables.
package config

import (
	"errors"
	"fmt"
	"log/slog"
	"net/url"
	"strings"
	"time"
)

// Config is the full runtime configuration. Secrets only ever come from the environment.
type Config struct {
	Addr      string
	PublicURL *url.URL
	DBPath    string
	// DatabaseURL, if set (postgres://…), is used instead of the SQLite file at DBPath.
	DatabaseURL string
	LogLevel    slog.Level
	Dev         bool
	TURNSecret  string
	TURNURLs    []string
	STUNURLs    []string
	// TURNTTL bounds TURN credential lifetime (0 = turn.DefaultTTL).
	TURNTTL time.Duration
	// AuditRetention is how long audit events (with IPs: personal data) are kept.
	AuditRetention time.Duration

	// AllowedOrigins are browser origins (besides PublicURL) allowed to make
	// cookie-authenticated requests, e.g. the Vite dev server.
	AllowedOrigins []string
	// TrustProxy uses the last X-Forwarded-For hop as the client IP (set behind Caddy).
	TrustProxy bool
	// WebDir, if set, is served as the viewer (static files with SPA fallback).
	WebDir string

	Google OAuthClient
	GitHub OAuthClient
}

// OAuthClient holds one identity provider's credentials. Empty ClientID disables it.
type OAuthClient struct {
	ClientID     string
	ClientSecret string
}

// Enabled reports whether the provider is configured.
func (c OAuthClient) Enabled() bool { return c.ClientID != "" && c.ClientSecret != "" }

// Load builds a Config from a getenv function (os.Getenv in production, a map in tests).
func Load(getenv func(string) string) (Config, error) {
	get := func(key, def string) string {
		if v := strings.TrimSpace(getenv(key)); v != "" {
			return v
		}
		return def
	}

	var errs []error
	cfg := Config{
		Addr:        get("CARSON_ADDR", ":8080"),
		DBPath:      get("CARSON_DB_PATH", "./data/carson.db"),
		DatabaseURL: get("CARSON_DATABASE_URL", ""),
		TURNSecret:  getenv("CARSON_TURN_SECRET"),
		TURNURLs:    splitList(getenv("CARSON_TURN_URLS")),
		STUNURLs:    splitList(getenv("CARSON_STUN_URLS")),

		AllowedOrigins: splitList(getenv("CARSON_ALLOWED_ORIGINS")),
		WebDir:         get("CARSON_WEB_DIR", ""),
		Google:         OAuthClient{getenv("CARSON_GOOGLE_CLIENT_ID"), getenv("CARSON_GOOGLE_CLIENT_SECRET")},
		GitHub:         OAuthClient{getenv("CARSON_GITHUB_CLIENT_ID"), getenv("CARSON_GITHUB_CLIENT_SECRET")},
	}

	parseBool := func(key string, dst *bool) {
		switch v := strings.ToLower(get(key, "false")); v {
		case "true", "1", "yes":
			*dst = true
		case "false", "0", "no":
		default:
			errs = append(errs, fmt.Errorf("%s: invalid boolean %q", key, v))
		}
	}
	if v := get("CARSON_TURN_TTL", ""); v != "" {
		d, err := time.ParseDuration(v)
		if err != nil || d < time.Minute || d > 48*time.Hour {
			errs = append(errs, fmt.Errorf("CARSON_TURN_TTL: want a duration between 1m and 48h, got %q", v))
		}
		cfg.TURNTTL = d
	}
	cfg.AuditRetention = 90 * 24 * time.Hour
	if v := get("CARSON_AUDIT_RETENTION", ""); v != "" {
		d, err := time.ParseDuration(v)
		if err != nil || d < time.Hour {
			errs = append(errs, fmt.Errorf("CARSON_AUDIT_RETENTION: want a duration of at least 1h (e.g. 2160h), got %q", v))
		}
		cfg.AuditRetention = d
	}
	if cfg.DatabaseURL != "" && !strings.HasPrefix(cfg.DatabaseURL, "postgres://") && !strings.HasPrefix(cfg.DatabaseURL, "postgresql://") {
		errs = append(errs, errors.New("CARSON_DATABASE_URL: must be a postgres:// or postgresql:// URL"))
	}
	parseBool("CARSON_DEV", &cfg.Dev)
	parseBool("CARSON_TRUST_PROXY", &cfg.TrustProxy)

	for _, o := range cfg.AllowedOrigins {
		if u, err := url.Parse(o); err != nil || u.Host == "" || u.Path != "" || (u.Scheme != "http" && u.Scheme != "https") {
			errs = append(errs, fmt.Errorf("CARSON_ALLOWED_ORIGINS: %q is not an origin like https://host[:port]", o))
		}
	}

	if err := cfg.LogLevel.UnmarshalText([]byte(get("CARSON_LOG_LEVEL", "info"))); err != nil {
		errs = append(errs, fmt.Errorf("CARSON_LOG_LEVEL: %w", err))
	}

	u, err := url.Parse(get("CARSON_PUBLIC_URL", "http://localhost:8080"))
	switch {
	case err != nil:
		errs = append(errs, fmt.Errorf("CARSON_PUBLIC_URL: %w", err))
	case u.Scheme != "http" && u.Scheme != "https", u.Host == "":
		errs = append(errs, fmt.Errorf("CARSON_PUBLIC_URL: must be an absolute http(s) URL, got %q", u))
	default:
		cfg.PublicURL = u
	}

	if !cfg.Dev {
		if cfg.PublicURL != nil && cfg.PublicURL.Scheme != "https" {
			errs = append(errs, errors.New("CARSON_PUBLIC_URL: must use https outside dev mode"))
		}
		if len(cfg.TURNSecret) < 32 {
			errs = append(errs, errors.New("CARSON_TURN_SECRET: must be at least 32 characters outside dev mode"))
		}
		if !cfg.Google.Enabled() && !cfg.GitHub.Enabled() {
			errs = append(errs, errors.New("no login provider configured: set CARSON_GOOGLE_* or CARSON_GITHUB_* (dev login is disabled outside dev mode)"))
		}
	}

	return cfg, errors.Join(errs...)
}

func splitList(s string) []string {
	var out []string
	for _, part := range strings.Split(s, ",") {
		if p := strings.TrimSpace(part); p != "" {
			out = append(out, p)
		}
	}
	return out
}
