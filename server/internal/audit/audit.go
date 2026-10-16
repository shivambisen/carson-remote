// Package audit records security-relevant events (sign-ins, pairing, revocation, every
// remote session) to the append-only audit log. Users can read their own log.
package audit

import (
	"context"
	"log/slog"
	"time"

	"carson/server/internal/store"
)

// DefaultRetention is how long events are kept (they contain IP addresses: personal data).
const DefaultRetention = 90 * 24 * time.Hour

// Log writes audit events. A nil *Log records nothing (tests, tools).
type Log struct {
	Store *store.Store
	Log   *slog.Logger
	Now   func() time.Time
}

// Record stores e (filling in the time). Failures are logged loudly, never ignored.
func (l *Log) Record(e store.AuditEvent) {
	if l == nil {
		return
	}
	if e.At.IsZero() {
		e.At = l.Now()
	}
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	if err := l.Store.AddAudit(ctx, e); err != nil {
		l.Log.Error("AUDIT WRITE FAILED", "action", e.Action, "user_id", e.UserID, "device_id", e.DeviceID, "sid", e.SID, "err", err)
	}
}
