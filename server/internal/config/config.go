// Package config loads carson-server settings from CARSON_* environment variables.
package config

import (
	"errors"
	"fmt"
	"log/slog"
	"net/url"
	"strings"
)

// Config is the full runtime configuration. Secrets only ever come from the environment.
type Config struct {
	Addr       string
	PublicURL  *url.URL
	DBPath     string
	LogLevel   slog.Level
	Dev        bool
	TURNSecret string
	TURNURLs   []string
	STUNURLs   []string
}

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
		Addr:       get("CARSON_ADDR", ":8080"),
		DBPath:     get("CARSON_DB_PATH", "./data/carson.db"),
		TURNSecret: getenv("CARSON_TURN_SECRET"),
		TURNURLs:   splitList(getenv("CARSON_TURN_URLS")),
		STUNURLs:   splitList(getenv("CARSON_STUN_URLS")),
	}

	switch v := strings.ToLower(get("CARSON_DEV", "false")); v {
	case "true", "1", "yes":
		cfg.Dev = true
	case "false", "0", "no":
	default:
		errs = append(errs, fmt.Errorf("CARSON_DEV: invalid boolean %q", v))
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
