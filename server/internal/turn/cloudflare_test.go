package turn

import (
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"
)

func TestCloudflareRelayCredentials(t *testing.T) {
	var gotTTL int64
	cf := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1/turn/keys/key123/credentials/generate-ice-servers" || r.Header.Get("Authorization") != "Bearer tok" {
			http.Error(w, "nope", http.StatusUnauthorized)
			return
		}
		var body struct{ TTL int64 }
		_ = json.NewDecoder(r.Body).Decode(&body)
		gotTTL = body.TTL
		w.WriteHeader(http.StatusCreated)
		_, _ = w.Write([]byte(`{"iceServers":[
			{"urls":["stun:stun.cloudflare.com:3478","stun:stun.cloudflare.com:53"]},
			{"urls":["turn:turn.cloudflare.com:3478?transport=udp","turn:turn.cloudflare.com:53?transport=udp","turns:turn.cloudflare.com:443?transport=tcp"],
			 "username":"u1","credential":"c1"}]}`))
	}))
	defer cf.Close()

	i := &Issuer{STUNURLs: []string{"stun:stun.l.google.com:19302"}, TTL: time.Hour, Cloudflare: &Cloudflare{KeyID: "key123", APIToken: "tok", API: cf.URL}}
	got := i.ICEServers("ses_1")
	if gotTTL != 3600 {
		t.Fatalf("ttl sent = %d", gotTTL)
	}
	if len(got) != 3 {
		t.Fatalf("servers = %+v", got)
	}
	relay := got[2]
	if relay.Username != "u1" || relay.Credential != "c1" || len(relay.URLs) != 2 || relay.URLs[1] != "turns:turn.cloudflare.com:443?transport=tcp" {
		t.Fatalf("relay = %+v (port 53 must be dropped, TLS/443 kept)", relay)
	}

	// A broken key: the session still gets STUN, and the error is reported.
	var reported error
	bad := &Issuer{STUNURLs: []string{"stun:x"}, Cloudflare: &Cloudflare{KeyID: "wrong", APIToken: "tok", API: cf.URL}, OnError: func(err error) { reported = err }}
	if s := bad.ICEServers("ses_2"); len(s) != 1 || reported == nil {
		t.Fatalf("fallback = %+v, err %v", s, reported)
	}
	if (&Cloudflare{}).Enabled() || errors.Is(reported, nil) {
		t.Fatal("empty config must be disabled")
	}
}
