package httpapi

import (
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"testing"
)

func newServer(t *testing.T) *httptest.Server {
	t.Helper()
	srv := httptest.NewServer(New(slog.New(slog.NewTextHandler(io.Discard, nil))))
	t.Cleanup(srv.Close)
	return srv
}

func TestHealthz(t *testing.T) {
	resp, err := http.Get(newServer(t).URL + "/healthz")
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("status = %d", resp.StatusCode)
	}
	var body map[string]string
	if err := json.NewDecoder(resp.Body).Decode(&body); err != nil {
		t.Fatal(err)
	}
	if body["status"] != "ok" {
		t.Fatalf("body = %v", body)
	}
}

func TestSecurityHeaders(t *testing.T) {
	resp, err := http.Get(newServer(t).URL + "/v1/version")
	if err != nil {
		t.Fatal(err)
	}
	resp.Body.Close()
	for _, h := range []string{"X-Content-Type-Options", "X-Frame-Options", "Content-Security-Policy"} {
		if resp.Header.Get(h) == "" {
			t.Errorf("missing header %s", h)
		}
	}
}

func TestMethodNotAllowed(t *testing.T) {
	resp, err := http.Post(newServer(t).URL+"/healthz", "text/plain", nil)
	if err != nil {
		t.Fatal(err)
	}
	resp.Body.Close()
	if resp.StatusCode != http.StatusMethodNotAllowed {
		t.Fatalf("status = %d, want 405", resp.StatusCode)
	}
}
