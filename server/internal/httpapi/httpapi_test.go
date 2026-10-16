package httpapi

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"net/http/cookiejar"
	"net/http/httptest"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/coder/websocket"

	"carson/server/internal/config"
	"carson/server/internal/store/storetest"
)

func newServer(t *testing.T, webDir string) *httptest.Server {
	t.Helper()
	st := storetest.Open(t)
	srv := httptest.NewUnstartedServer(nil)
	srv.Start()
	t.Cleanup(srv.Close)
	pub, _ := url.Parse(srv.URL)
	cfg := config.Config{PublicURL: pub, Dev: true, WebDir: webDir}
	srv.Config.Handler = New(cfg, st, slog.New(slog.NewTextHandler(io.Discard, nil))).Handler
	return srv
}

func TestHealthAndReadiness(t *testing.T) {
	srv := newServer(t, "")
	for _, path := range []string{"/healthz", "/readyz", "/v1/version"} {
		resp, err := http.Get(srv.URL + path)
		if err != nil {
			t.Fatal(err)
		}
		resp.Body.Close()
		if resp.StatusCode != http.StatusOK {
			t.Errorf("%s = %d", path, resp.StatusCode)
		}
	}
}

func TestSecurityHeaders(t *testing.T) {
	resp, err := http.Get(newServer(t, "").URL + "/v1/version")
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
	resp, err := http.Post(newServer(t, "").URL+"/healthz", "text/plain", nil)
	if err != nil {
		t.Fatal(err)
	}
	resp.Body.Close()
	if resp.StatusCode != http.StatusMethodNotAllowed {
		t.Fatalf("status = %d, want 405", resp.StatusCode)
	}
}

func TestServesViewerFromWebDir(t *testing.T) {
	dir := t.TempDir()
	_ = os.WriteFile(filepath.Join(dir, "index.html"), []byte("<title>carson</title>"), 0o600)
	resp, err := http.Get(newServer(t, dir).URL + "/")
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(resp.Body)
	if !strings.Contains(string(body), "carson") {
		t.Fatalf("body = %q", body)
	}
}

// End-to-end through the real middleware stack: dev login, then a viewer WebSocket.
func TestViewerWebSocketThroughMiddleware(t *testing.T) {
	srv := newServer(t, "")
	jar, _ := cookiejar.New(nil)
	client := &http.Client{Jar: jar}
	req, _ := http.NewRequest(http.MethodPost, srv.URL+"/auth/dev/login", bytes.NewBufferString(`{"email":"ana@x.com"}`))
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Origin", srv.URL)
	resp, err := client.Do(req)
	if err != nil || resp.StatusCode != http.StatusOK {
		t.Fatalf("dev login: %v %v", err, resp)
	}
	resp.Body.Close()

	u, _ := url.Parse(srv.URL)
	h := http.Header{"Origin": {srv.URL}}
	for _, c := range jar.Cookies(u) {
		h.Add("Cookie", c.String())
	}
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	conn, _, err := websocket.Dial(ctx, "ws"+strings.TrimPrefix(srv.URL, "http")+"/v1/ws", &websocket.DialOptions{HTTPHeader: h})
	if err != nil {
		t.Fatal(err)
	}
	defer conn.CloseNow()
	_, data, err := conn.Read(ctx)
	if err != nil {
		t.Fatal(err)
	}
	var env struct{ Type string }
	_ = json.Unmarshal(data, &env)
	if env.Type != "hello.ok" {
		t.Fatalf("first message = %s", data)
	}
}
