package store

import (
	"context"
	"database/sql"
	"errors"
	"strings"
	"time"
)

// User is a carson account. Devices and sessions belong to exactly one user.
type User struct {
	ID          string    `json:"id"`
	Email       string    `json:"email"`
	DisplayName string    `json:"display_name"`
	CreatedAt   time.Time `json:"created_at"`
}

// Identity is an external login asserted by an identity provider.
type Identity struct {
	Provider string
	Subject  string
	Email    string // must be verified by the provider
	Name     string
}

// UpsertUserForIdentity returns the user linked to ident, creating it on first login.
// A new identity is linked to an existing user with the same (provider-verified) email,
// so signing in with Google and GitHub under one address yields one carson account.
func (s *Store) UpsertUserForIdentity(ctx context.Context, ident Identity, now time.Time) (User, error) {
	email := strings.ToLower(strings.TrimSpace(ident.Email))
	if ident.Provider == "" || ident.Subject == "" || email == "" {
		return User{}, errors.New("identity requires provider, subject and verified email")
	}
	var u User
	err := s.tx(ctx, func(tx *Tx) error {
		var userID string
		err := tx.QueryRowContext(ctx, `SELECT user_id FROM identities WHERE provider = ? AND subject = ?`,
			ident.Provider, ident.Subject).Scan(&userID)
		switch {
		case err == nil:
		case errors.Is(err, sql.ErrNoRows):
			err = tx.QueryRowContext(ctx, `SELECT id FROM users WHERE email = ?`, email).Scan(&userID)
			if errors.Is(err, sql.ErrNoRows) {
				userID = NewID("usr")
				_, err = tx.ExecContext(ctx, `INSERT INTO users (id, email, display_name, created_at) VALUES (?, ?, ?, ?)`,
					userID, email, ident.Name, ms(now))
			}
			if err != nil {
				return err
			}
			if _, err := tx.ExecContext(ctx, `INSERT INTO identities (provider, subject, user_id, email, created_at) VALUES (?, ?, ?, ?, ?)`,
				ident.Provider, ident.Subject, userID, email, ms(now)); err != nil {
				return err
			}
		default:
			return err
		}
		var created int64
		if err := tx.QueryRowContext(ctx, `SELECT id, email, display_name, created_at FROM users WHERE id = ?`, userID).
			Scan(&u.ID, &u.Email, &u.DisplayName, &created); err != nil {
			return err
		}
		u.CreatedAt = fromMS(created)
		return nil
	})
	return u, err
}

// CreateWebSession stores a browser session keyed by the hash of its cookie value.
func (s *Store) CreateWebSession(ctx context.Context, tokenHash []byte, userID string, now, expires time.Time, ip, userAgent string) error {
	if len(userAgent) > 256 {
		userAgent = userAgent[:256]
	}
	_, err := s.db.ExecContext(ctx, `INSERT INTO web_sessions (token_hash, user_id, created_at, expires_at, ip, user_agent) VALUES (?, ?, ?, ?, ?, ?)`,
		tokenHash, userID, ms(now), ms(expires), ip, userAgent)
	return err
}

// UserForWebSession resolves an unexpired session to its user.
func (s *Store) UserForWebSession(ctx context.Context, tokenHash []byte, now time.Time) (User, error) {
	var u User
	var created int64
	err := s.db.QueryRowContext(ctx, `
		SELECT u.id, u.email, u.display_name, u.created_at
		FROM web_sessions s JOIN users u ON u.id = s.user_id
		WHERE s.token_hash = ? AND s.expires_at > ?`, tokenHash, ms(now)).
		Scan(&u.ID, &u.Email, &u.DisplayName, &created)
	if errors.Is(err, sql.ErrNoRows) {
		return User{}, ErrNotFound
	}
	u.CreatedAt = fromMS(created)
	return u, err
}

func (s *Store) DeleteWebSession(ctx context.Context, tokenHash []byte) error {
	_, err := s.db.ExecContext(ctx, `DELETE FROM web_sessions WHERE token_hash = ?`, tokenHash)
	return err
}

// OAuthState is a pending login; it is single-use.
type OAuthState struct {
	State    string
	Provider string
	Verifier string
	Nonce    string
	ReturnTo string
	Expires  time.Time
}

func (s *Store) PutOAuthState(ctx context.Context, st OAuthState) error {
	_, err := s.db.ExecContext(ctx, `INSERT INTO oauth_states (state, provider, verifier, nonce, return_to, expires_at) VALUES (?, ?, ?, ?, ?, ?)`,
		st.State, st.Provider, st.Verifier, st.Nonce, st.ReturnTo, ms(st.Expires))
	return err
}

// TakeOAuthState deletes and returns the state if it exists and has not expired.
func (s *Store) TakeOAuthState(ctx context.Context, state string, now time.Time) (OAuthState, error) {
	var st OAuthState
	var expires int64
	err := s.db.QueryRowContext(ctx, `DELETE FROM oauth_states WHERE state = ? RETURNING state, provider, verifier, nonce, return_to, expires_at`, state).
		Scan(&st.State, &st.Provider, &st.Verifier, &st.Nonce, &st.ReturnTo, &expires)
	if errors.Is(err, sql.ErrNoRows) {
		return OAuthState{}, ErrNotFound
	}
	if err != nil {
		return OAuthState{}, err
	}
	st.Expires = fromMS(expires)
	if !now.Before(st.Expires) {
		return OAuthState{}, ErrNotFound
	}
	return st, nil
}

// DeleteExpired removes expired sessions, logins, pairings and device tokens.
func (s *Store) DeleteExpired(ctx context.Context, now time.Time) error {
	for _, q := range []string{
		`DELETE FROM web_sessions WHERE expires_at <= ?`,
		`DELETE FROM oauth_states WHERE expires_at <= ?`,
		`DELETE FROM pairings WHERE expires_at <= ?`,
		`DELETE FROM device_tokens WHERE expires_at <= ?`,
	} {
		if _, err := s.db.ExecContext(ctx, q, ms(now)); err != nil {
			return err
		}
	}
	return nil
}
