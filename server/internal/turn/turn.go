// Package turn mints time-limited TURN credentials in coturn's "TURN REST API" scheme
// (coturn: use-auth-secret + static-auth-secret). The server and coturn share only the
// secret; coturn validates credentials without calling back.
package turn

import (
	"context"
	"crypto/hmac"
	"crypto/sha1"
	"encoding/base64"
	"strconv"
	"time"

	"carson/server/internal/protocol"
)

// DefaultTTL bounds how long one session's credentials work. TURN allocations are refreshed
// every few minutes with the same credentials, so this must outlast a long session;
// a leaked credential only allows relaying (through coturn's denied-peer filters) until expiry.
const DefaultTTL = 6 * time.Hour

// Issuer builds the ICE server list for a session.
type Issuer struct {
	Secret   string
	TURNURLs []string
	STUNURLs []string
	TTL      time.Duration
	Now      func() time.Time
	// Cloudflare, if configured, provides the relay instead of (or besides) coturn.
	Cloudflare *Cloudflare
	// StaticTURNURLs, with StaticUsername and StaticCredential: a relay that hands out one fixed
	// username/credential pair rather than minting one per session (e.g. a provider's free tier,
	// such as Metered's "Open Relay"). Sent to every session, so treat it as shared, not secret.
	StaticTURNURLs                   []string
	StaticUsername, StaticCredential string
	// OnError reports relay credential failures (the session continues without a relay).
	OnError func(error)
}

// Credentials returns username "<unix expiry>:<label>" and base64(HMAC-SHA1(secret, username)).
func Credentials(secret, label string, expires time.Time) (username, credential string) {
	username = strconv.FormatInt(expires.Unix(), 10) + ":" + label
	mac := hmac.New(sha1.New, []byte(secret)) // SHA-1 is what coturn's REST scheme specifies
	mac.Write([]byte(username))
	return username, base64.StdEncoding.EncodeToString(mac.Sum(nil))
}

// ICEServers returns STUN (if configured) plus TURN with fresh credentials scoped to `label`
// (the session ID, so a credential can be traced to a session in coturn's logs).
func (i *Issuer) ICEServers(label string) []protocol.ICEServer {
	servers := []protocol.ICEServer{}
	if len(i.STUNURLs) > 0 {
		servers = append(servers, protocol.ICEServer{URLs: i.STUNURLs})
	}
	if len(i.TURNURLs) > 0 && i.Secret != "" {
		ttl := i.TTL
		if ttl <= 0 {
			ttl = DefaultTTL
		}
		now := time.Now
		if i.Now != nil {
			now = i.Now
		}
		user, cred := Credentials(i.Secret, label, now().Add(ttl))
		servers = append(servers, protocol.ICEServer{URLs: i.TURNURLs, Username: user, Credential: cred})
	}
	if len(i.StaticTURNURLs) > 0 && i.StaticUsername != "" && i.StaticCredential != "" {
		servers = append(servers, protocol.ICEServer{
			URLs: i.StaticTURNURLs, Username: i.StaticUsername, Credential: i.StaticCredential,
		})
	}
	if i.Cloudflare.Enabled() {
		ttl := i.TTL
		if ttl <= 0 || ttl > 48*time.Hour {
			ttl = DefaultTTL
		}
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		cf, err := i.Cloudflare.ICEServers(ctx, ttl)
		if err != nil {
			if i.OnError != nil {
				i.OnError(err)
			}
		} else {
			servers = append(servers, cf...)
		}
	}
	return servers
}
