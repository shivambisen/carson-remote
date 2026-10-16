package store

import (
	"context"
	"database/sql"

	"time"
)

// Audit actions.
const (
	AuditSignedIn        = "signed_in"
	AuditDevicePaired    = "device_paired"
	AuditDeviceRevoked   = "device_revoked"
	AuditSessionRequest  = "session_requested"
	AuditSessionAccepted = "session_accepted"
	AuditSessionRejected = "session_rejected"
	AuditSessionResumed  = "session_resumed"
	AuditSessionEnded    = "session_ended"
)

// AuditEvent is one row of the security log.
type AuditEvent struct {
	ID         int64     `json:"id"`
	At         time.Time `json:"at"`
	UserID     string    `json:"-"`
	DeviceID   string    `json:"device_id,omitempty"`
	DeviceName string    `json:"device_name,omitempty"`
	SID        string    `json:"sid,omitempty"`
	Action     string    `json:"action"`
	IP         string    `json:"ip,omitempty"`
	UserAgent  string    `json:"user_agent,omitempty"`
	Detail     string    `json:"detail,omitempty"`
}

func clip(s string, n int) string {
	if len(s) > n {
		return s[:n]
	}
	return s
}

// AddAudit appends an event. Long fields are truncated, never rejected.
func (s *Store) AddAudit(ctx context.Context, e AuditEvent) error {
	_, err := s.db.ExecContext(ctx, `INSERT INTO audit_log (at, user_id, device_id, sid, action, ip, user_agent, detail) VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
		ms(e.At), e.UserID, e.DeviceID, e.SID, e.Action, clip(e.IP, 64), clip(e.UserAgent, 256), clip(e.Detail, 512))
	return err
}

// ListAudit returns the user's newest events (before `beforeID` if > 0), newest first.
func (s *Store) ListAudit(ctx context.Context, userID string, beforeID int64, limit int) ([]AuditEvent, error) {
	if limit <= 0 || limit > 200 {
		limit = 100
	}
	if beforeID <= 0 {
		beforeID = 1<<63 - 1
	}
	rows, err := s.db.QueryContext(ctx, `
		SELECT a.id, a.at, a.user_id, a.device_id, COALESCE(d.name, ''), a.sid, a.action, a.ip, a.user_agent, a.detail
		FROM audit_log a LEFT JOIN devices d ON d.id = a.device_id
		WHERE a.user_id = ? AND a.id < ?
		ORDER BY a.id DESC LIMIT ?`, userID, beforeID, limit)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []AuditEvent{}
	for rows.Next() {
		var e AuditEvent
		var at int64
		if err := rows.Scan(&e.ID, &at, &e.UserID, &e.DeviceID, &e.DeviceName, &e.SID, &e.Action, &e.IP, &e.UserAgent, &e.Detail); err != nil {
			return nil, err
		}
		e.At = fromMS(at)
		out = append(out, e)
	}
	return out, rows.Err()
}

// PruneAudit deletes events older than `before` (retention).
func (s *Store) PruneAudit(ctx context.Context, before time.Time) error {
	_, err := s.db.ExecContext(ctx, `DELETE FROM audit_log WHERE at < ?`, ms(before))
	return err
}

// RevokeDevice marks the user's device revoked and deletes its access tokens. Revoked devices
// stay in the table (so the audit log can still name them) but can never authenticate again.
func (s *Store) RevokeDevice(ctx context.Context, userID, deviceID string, now time.Time) error {
	return s.tx(ctx, func(tx *sql.Tx) error {
		res, err := tx.ExecContext(ctx, `UPDATE devices SET revoked_at = ? WHERE id = ? AND user_id = ? AND revoked_at IS NULL`, ms(now), deviceID, userID)
		if err != nil {
			return err
		}
		if n, _ := res.RowsAffected(); n == 0 {
			return ErrNotFound
		}
		_, err = tx.ExecContext(ctx, `DELETE FROM device_tokens WHERE device_id = ?`, deviceID)
		return err
	})
}
