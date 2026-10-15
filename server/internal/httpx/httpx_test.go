package httpx

import (
	"net/http/httptest"
	"strings"
	"testing"
)

func TestClientIP(t *testing.T) {
	r := httptest.NewRequest("GET", "/", nil)
	r.RemoteAddr = "10.0.0.5:1234"
	r.Header.Set("X-Forwarded-For", "6.6.6.6, 203.0.113.9")
	if got := ClientIP(r, false); got != "10.0.0.5" {
		t.Errorf("untrusted proxy: got %s", got)
	}
	if got := ClientIP(r, true); got != "203.0.113.9" {
		t.Errorf("trusted proxy: got %s (must use last hop, not client-supplied first)", got)
	}
	r.Header.Set("X-Forwarded-For", "garbage")
	if got := ClientIP(r, true); got != "10.0.0.5" {
		t.Errorf("bad XFF: got %s", got)
	}
}

func TestDecodeJSON(t *testing.T) {
	var dst struct{ A int }
	cases := map[string]struct {
		ct, body string
		ok       bool
	}{
		"valid":         {"application/json", `{"A":1}`, true},
		"wrong type":    {"text/plain", `{"A":1}`, false},
		"unknown field": {"application/json", `{"B":1}`, false},
		"trailing":      {"application/json", `{"A":1}{}`, false},
		"too large":     {"application/json", `{"A":"` + strings.Repeat("x", MaxBodyBytes) + `"}`, false},
	}
	for name, c := range cases {
		t.Run(name, func(t *testing.T) {
			r := httptest.NewRequest("POST", "/", strings.NewReader(c.body))
			r.Header.Set("Content-Type", c.ct)
			err := DecodeJSON(httptest.NewRecorder(), r, &dst)
			if (err == nil) != c.ok {
				t.Fatalf("err = %v, want ok=%v", err, c.ok)
			}
		})
	}
}
