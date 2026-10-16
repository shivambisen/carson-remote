package turn

import (
	"strings"
	"testing"
	"time"
)

func TestCredentialsMatchIndependentVector(t *testing.T) {
	// Computed with Python: base64(hmac.new(secret, "1800000000:ses_test", sha1).digest()).
	user, cred := Credentials("test-secret-0123456789abcdef0123456789", "ses_test", time.Unix(1800000000, 0))
	if user != "1800000000:ses_test" || cred != "nzD+yKPEty1GLht5dLxPkkVNF2Q=" {
		t.Fatalf("got %s / %s", user, cred)
	}
}

func TestICEServers(t *testing.T) {
	now := time.Unix(1700000000, 0)
	i := &Issuer{
		Secret: "s3cret", TURNURLs: []string{"turn:t.example:3478?transport=udp", "turns:t.example:443"},
		STUNURLs: []string{"stun:t.example:3478"}, TTL: time.Hour, Now: func() time.Time { return now },
	}
	got := i.ICEServers("ses_1")
	if len(got) != 2 || got[0].URLs[0] != "stun:t.example:3478" || got[0].Username != "" {
		t.Fatalf("stun entry: %+v", got)
	}
	if got[1].Username != "1700003600:ses_1" || got[1].Credential == "" || len(got[1].URLs) != 2 {
		t.Fatalf("turn entry: %+v", got[1])
	}
	if !strings.HasPrefix(got[1].Username, "1700003600") {
		t.Fatal("expiry must be now + TTL")
	}

	if s := (&Issuer{TURNURLs: []string{"turn:x"}}).ICEServers("ses"); len(s) != 0 {
		t.Fatalf("TURN without a secret must be omitted, got %+v", s)
	}
}
