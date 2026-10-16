// Package turn mints time-limited TURN credentials in coturn's "TURN REST API" scheme
// (coturn: use-auth-secret + static-auth-secret). The server and coturn share only the
// secret; coturn validates credentials without calling back.
package turn

import (
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
	return servers
}
