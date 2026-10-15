package store

import (
	"context"
	"database/sql"
	"errors"
	"time"
)

// Device is a host running carson-agent, owned by one user.
type Device struct {
	ID           string     `json:"id"`
	UserID       string     `json:"-"`
	Name         string     `json:"name"`
	OS           string     `json:"os"`
	PublicKey    []byte     `json:"-"`
	AgentVersion string     `json:"agent_version"`
	CreatedAt    time.Time  `json:"created_at"`
	LastSeenAt   time.Time  `json:"last_seen_at"`
	RevokedAt    *time.Time `json:"revoked_at,omitempty"`
}

const deviceCols = `id, user_id, name, os, public_key, agent_version, created_at, last_seen_at, revoked_at`

func scanDevice(row interface{ Scan(...any) error }) (Device, error) {
	var d Device
	var created, seen int64
	var revoked sql.NullInt64
	if err := row.Scan(&d.ID, &d.UserID, &d.Name, &d.OS, &d.PublicKey, &d.AgentVersion, &created, &seen, &revoked); err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return Device{}, ErrNotFound
		}
		return Device{}, err
	}
	d.CreatedAt, d.LastSeenAt = fromMS(created), fromMS(seen)
	if revoked.Valid {
		t := fromMS(revoked.Int64)
		d.RevokedAt = &t
	}
	return d, nil
}

// ListDevices returns the user's non-revoked devices, oldest first.
func (s *Store) ListDevices(ctx context.Context, userID string) ([]Device, error) {
	rows, err := s.db.QueryContext(ctx, `SELECT `+deviceCols+` FROM devices WHERE user_id = ? AND revoked_at IS NULL ORDER BY created_at, id`, userID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	devices := []Device{}
	for rows.Next() {
		d, err := scanDevice(rows)
		if err != nil {
			return nil, err
		}
		devices = append(devices, d)
	}
	return devices, rows.Err()
}

// GetDevice returns a device by ID, including revoked ones (callers must check RevokedAt).
func (s *Store) GetDevice(ctx context.Context, id string) (Device, error) {
	return scanDevice(s.db.QueryRowContext(ctx, `SELECT `+deviceCols+` FROM devices WHERE id = ?`, id))
}

// TouchDevice records a heartbeat and the agent's reported version/OS.
func (s *Store) TouchDevice(ctx context.Context, id string, seen time.Time, agentVersion, os string) error {
	_, err := s.db.ExecContext(ctx, `
		UPDATE devices SET last_seen_at = ?,
			agent_version = COALESCE(NULLIF(?, ''), agent_version),
			os = COALESCE(NULLIF(?, ''), os)
		WHERE id = ?`, ms(seen), agentVersion, os, id)
	return err
}

// Pairing is an enrollment request from an agent awaiting approval by a signed-in user.
type Pairing struct {
	ID        string
	CodeHash  []byte
	PublicKey []byte
	Name      string
	OS        string
	Status    string // "pending" or "approved"
	UserID    string
	DeviceID  string
	CreatedAt time.Time
	ExpiresAt time.Time
}

const pairingCols = `id, code_hash, public_key, name, os, status, COALESCE(user_id, ''), COALESCE(device_id, ''), created_at, expires_at`

func scanPairing(row interface{ Scan(...any) error }) (Pairing, error) {
	var p Pairing
	var created, expires int64
	err := row.Scan(&p.ID, &p.CodeHash, &p.PublicKey, &p.Name, &p.OS, &p.Status, &p.UserID, &p.DeviceID, &created, &expires)
	if errors.Is(err, sql.ErrNoRows) {
		return Pairing{}, ErrNotFound
	}
	p.CreatedAt, p.ExpiresAt = fromMS(created), fromMS(expires)
	return p, err
}

func (s *Store) CreatePairing(ctx context.Context, p Pairing) error {
	_, err := s.db.ExecContext(ctx, `INSERT INTO pairings (id, code_hash, public_key, name, os, status, created_at, expires_at) VALUES (?, ?, ?, ?, ?, 'pending', ?, ?)`,
		p.ID, p.CodeHash, p.PublicKey, p.Name, p.OS, ms(p.CreatedAt), ms(p.ExpiresAt))
	if isUniqueViolation(err) {
		return ErrConflict
	}
	return err
}

// PendingPairingByCode finds an unexpired, not-yet-approved pairing by its code hash.
func (s *Store) PendingPairingByCode(ctx context.Context, codeHash []byte, now time.Time) (Pairing, error) {
	return scanPairing(s.db.QueryRowContext(ctx, `SELECT `+pairingCols+` FROM pairings WHERE code_hash = ? AND status = 'pending' AND expires_at > ?`, codeHash, ms(now)))
}

// GetPairing returns an unexpired pairing by ID.
func (s *Store) GetPairing(ctx context.Context, id string, now time.Time) (Pairing, error) {
	return scanPairing(s.db.QueryRowContext(ctx, `SELECT `+pairingCols+` FROM pairings WHERE id = ? AND expires_at > ?`, id, ms(now)))
}

// ApprovePairing binds a pending pairing to userID and creates the device, atomically.
func (s *Store) ApprovePairing(ctx context.Context, codeHash []byte, userID string, now time.Time) (Device, error) {
	var d Device
	err := s.tx(ctx, func(tx *sql.Tx) error {
		p, err := scanPairing(tx.QueryRowContext(ctx, `SELECT `+pairingCols+` FROM pairings WHERE code_hash = ? AND status = 'pending' AND expires_at > ?`, codeHash, ms(now)))
		if err != nil {
			return err
		}
		d = Device{ID: NewID("dev"), UserID: userID, Name: p.Name, OS: p.OS, PublicKey: p.PublicKey, CreatedAt: now}
		_, err = tx.ExecContext(ctx, `INSERT INTO devices (id, user_id, name, os, public_key, created_at) VALUES (?, ?, ?, ?, ?, ?)`,
			d.ID, d.UserID, d.Name, d.OS, d.PublicKey, ms(now))
		if isUniqueViolation(err) {
			return ErrConflict // this key is already enrolled
		}
		if err != nil {
			return err
		}
		_, err = tx.ExecContext(ctx, `UPDATE pairings SET status = 'approved', user_id = ?, device_id = ? WHERE id = ?`, userID, d.ID, p.ID)
		return err
	})
	return d, err
}

func (s *Store) CreateDeviceToken(ctx context.Context, tokenHash []byte, deviceID string, expires time.Time) error {
	_, err := s.db.ExecContext(ctx, `INSERT INTO device_tokens (token_hash, device_id, expires_at) VALUES (?, ?, ?)`, tokenHash, deviceID, ms(expires))
	return err
}

// DeviceForToken resolves an unexpired access token to its (non-revoked) device.
func (s *Store) DeviceForToken(ctx context.Context, tokenHash []byte, now time.Time) (Device, error) {
	return scanDevice(s.db.QueryRowContext(ctx, `
		SELECT d.id, d.user_id, d.name, d.os, d.public_key, d.agent_version, d.created_at, d.last_seen_at, d.revoked_at
		FROM device_tokens t JOIN devices d ON d.id = t.device_id
		WHERE t.token_hash = ? AND t.expires_at > ? AND d.revoked_at IS NULL`, tokenHash, ms(now)))
}
