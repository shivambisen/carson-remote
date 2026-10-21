package config

import (
	"log/slog"
	"slices"
	"strings"
	"testing"
)

func env(m map[string]string) func(string) string {
	return func(k string) string { return m[k] }
}

func TestLoadDevDefaults(t *testing.T) {
	cfg, err := Load(env(map[string]string{"CARSON_DEV": "true"}))
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if cfg.Addr != ":8080" || !cfg.Dev || cfg.LogLevel != slog.LevelInfo {
		t.Fatalf("unexpected defaults: %+v", cfg)
	}
	if cfg.PublicURL.String() != "http://localhost:8080" {
		t.Fatalf("public url = %s", cfg.PublicURL)
	}
}

func TestLoadParsesLists(t *testing.T) {
	cfg, err := Load(env(map[string]string{
		"CARSON_DEV":       "1",
		"CARSON_TURN_URLS": " turn:a:3478?transport=udp , ,turn:a:3478?transport=tcp",
	}))
	if err != nil {
		t.Fatal(err)
	}
	want := []string{"turn:a:3478?transport=udp", "turn:a:3478?transport=tcp"}
	if !slices.Equal(cfg.TURNURLs, want) {
		t.Fatalf("TURNURLs = %q, want %q", cfg.TURNURLs, want)
	}
}

func TestLoadRejectsInsecureProduction(t *testing.T) {
	_, err := Load(env(map[string]string{
		"CARSON_PUBLIC_URL":  "http://carson.example.com",
		"CARSON_TURN_SECRET": "short",
	}))
	if err == nil {
		t.Fatal("expected error for http URL and short secret outside dev mode")
	}
	for _, want := range []string{"must use https", "at least 32 characters"} {
		if !strings.Contains(err.Error(), want) {
			t.Errorf("error %q missing %q", err, want)
		}
	}
}

func TestLoadAcceptsValidProduction(t *testing.T) {
	cfg, err := Load(env(map[string]string{
		"CARSON_PUBLIC_URL":           "https://carson.example.com",
		"CARSON_TURN_SECRET":          strings.Repeat("x", 32),
		"CARSON_GITHUB_CLIENT_ID":     "id",
		"CARSON_GITHUB_CLIENT_SECRET": "secret",
		"CARSON_TRUST_PROXY":          "true",
	}))
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !cfg.GitHub.Enabled() || cfg.Google.Enabled() || !cfg.TrustProxy {
		t.Fatalf("unexpected config: %+v", cfg)
	}
}

func TestLoadRequiresLoginProviderInProduction(t *testing.T) {
	_, err := Load(env(map[string]string{
		"CARSON_PUBLIC_URL":  "https://carson.example.com",
		"CARSON_TURN_SECRET": strings.Repeat("x", 32),
	}))
	if err == nil || !strings.Contains(err.Error(), "no login provider") {
		t.Fatalf("err = %v", err)
	}
}

func TestLoadRejectsBadValues(t *testing.T) {
	cases := map[string]map[string]string{
		"bad bool":     {"CARSON_DEV": "maybe"},
		"bad level":    {"CARSON_DEV": "true", "CARSON_LOG_LEVEL": "loud"},
		"relative url": {"CARSON_DEV": "true", "CARSON_PUBLIC_URL": "/carson"},
		"ftp url":      {"CARSON_DEV": "true", "CARSON_PUBLIC_URL": "ftp://x"},
		"origin path":  {"CARSON_DEV": "true", "CARSON_ALLOWED_ORIGINS": "http://localhost:5173/app"},
		"bad proxy":    {"CARSON_DEV": "true", "CARSON_TRUST_PROXY": "sometimes"},
	}
	for name, m := range cases {
		t.Run(name, func(t *testing.T) {
			if _, err := Load(env(m)); err == nil {
				t.Fatal("expected error")
			}
		})
	}
}

func TestAddrFollowsPlatformPort(t *testing.T) {
	env := map[string]string{"CARSON_DEV": "true", "PORT": "10000"}
	cfg, err := Load(func(k string) string { return env[k] })
	if err != nil || cfg.Addr != ":10000" {
		t.Fatalf("Addr = %q, %v", cfg.Addr, err)
	}
	env["CARSON_ADDR"] = "127.0.0.1:9000"
	if cfg, _ := Load(func(k string) string { return env[k] }); cfg.Addr != "127.0.0.1:9000" {
		t.Fatalf("CARSON_ADDR must win, got %q", cfg.Addr)
	}
}

func TestLoadAltPublicURLs(t *testing.T) {
	cfg, err := Load(env(map[string]string{"CARSON_DEV": "1", "CARSON_ALT_PUBLIC_URLS": "https://x.onrender.com/, https://y.example"}))
	if err != nil {
		t.Fatal(err)
	}
	if len(cfg.AltPublicURLs) != 2 || cfg.AltPublicURLs[0].String() != "https://x.onrender.com" {
		t.Fatalf("AltPublicURLs = %v", cfg.AltPublicURLs)
	}
	if _, err := Load(env(map[string]string{"CARSON_DEV": "1", "CARSON_ALT_PUBLIC_URLS": "https://x/path"})); err == nil {
		t.Fatal("accepted an address with a path")
	}
}
