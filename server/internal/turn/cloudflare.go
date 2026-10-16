package turn

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"

	"carson/server/internal/protocol"
)

// Cloudflare mints short-lived credentials for Cloudflare's TURN relay (Realtime TURN). Its
// relay also answers on TCP/TLS port 443, which gets through campus and office firewalls that
// block direct (UDP) connections. Free allowance: about 1,000 GB of relayed traffic a month.
type Cloudflare struct {
	KeyID    string
	APIToken string
	API      string // default https://rtc.live.cloudflare.com (tests use a fake)
	HTTP     *http.Client
}

// Enabled reports whether a TURN key is configured.
func (c *Cloudflare) Enabled() bool { return c != nil && c.KeyID != "" && c.APIToken != "" }

// ICEServers asks Cloudflare for relay credentials valid for ttl.
func (c *Cloudflare) ICEServers(ctx context.Context, ttl time.Duration) ([]protocol.ICEServer, error) {
	api := c.API
	if api == "" {
		api = "https://rtc.live.cloudflare.com"
	}
	body, _ := json.Marshal(map[string]int64{"ttl": int64(ttl.Seconds())})
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, api+"/v1/turn/keys/"+c.KeyID+"/credentials/generate-ice-servers", bytes.NewReader(body))
	if err != nil {
		return nil, err
	}
	req.Header.Set("Authorization", "Bearer "+c.APIToken)
	req.Header.Set("Content-Type", "application/json")
	client := c.HTTP
	if client == nil {
		client = &http.Client{Timeout: 5 * time.Second}
	}
	resp, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	raw, _ := io.ReadAll(io.LimitReader(resp.Body, 1<<16))
	if resp.StatusCode != http.StatusOK && resp.StatusCode != http.StatusCreated {
		return nil, fmt.Errorf("cloudflare turn: HTTP %d: %s", resp.StatusCode, raw)
	}
	var out struct {
		ICEServers []struct {
			URLs       json.RawMessage `json:"urls"`
			Username   string          `json:"username"`
			Credential string          `json:"credential"`
		} `json:"iceServers"`
	}
	if err := json.Unmarshal(raw, &out); err != nil {
		return nil, fmt.Errorf("cloudflare turn: %w", err)
	}
	var servers []protocol.ICEServer
	for _, s := range out.ICEServers {
		var urls []string
		if json.Unmarshal(s.URLs, &urls) != nil {
			var one string
			if json.Unmarshal(s.URLs, &one) != nil {
				continue
			}
			urls = []string{one}
		}
		// Browsers block port 53; those entries only make connection setup slower.
		kept := urls[:0]
		for _, u := range urls {
			if !strings.Contains(u, ":53?") && !strings.HasSuffix(u, ":53") {
				kept = append(kept, u)
			}
		}
		if len(kept) > 0 {
			servers = append(servers, protocol.ICEServer{URLs: kept, Username: s.Username, Credential: s.Credential})
		}
	}
	if len(servers) == 0 {
		return nil, fmt.Errorf("cloudflare turn: no usable servers in %s", raw)
	}
	return servers, nil
}
