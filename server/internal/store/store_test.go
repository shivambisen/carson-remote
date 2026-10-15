package store

import (
	"context"
	"errors"
	"path/filepath"
	"testing"
	"time"
)

var t0 = time.Date(2026, 10, 1, 12, 0, 0, 0, time.UTC)

func newStore(t *testing.T) *Store {
	t.Helper()
	s, err := Open(context.Background(), filepath.Join(t.TempDir(), "carson.db"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { s.Close() })
	return s
}

func TestMigrationsAreIdempotent(t *testing.T) {
	path := filepath.Join(t.TempDir(), "carson.db")
	for range 2 {
		s, err := Open(context.Background(), path)
		if err != nil {
			t.Fatal(err)
		}
		s.Close()
	}
}

func TestIdentityLinkingByVerifiedEmail(t *testing.T) {
	s, ctx := newStore(t), context.Background()
	a, err := s.UpsertUserForIdentity(ctx, Identity{Provider: "google", Subject: "1", Email: "Ana@Example.com", Name: "Ana"}, t0)
	if err != nil {
		t.Fatal(err)
	}
	again, _ := s.UpsertUserForIdentity(ctx, Identity{Provider: "google", Subject: "1", Email: "ana@example.com"}, t0)
	gh, _ := s.UpsertUserForIdentity(ctx, Identity{Provider: "github", Subject: "99", Email: "ana@example.com"}, t0)
	other, _ := s.UpsertUserForIdentity(ctx, Identity{Provider: "google", Subject: "2", Email: "bo@example.com"}, t0)

	if a.ID != again.ID || a.ID != gh.ID {
		t.Fatalf("same email should map to one user: %s %s %s", a.ID, again.ID, gh.ID)
	}
	if other.ID == a.ID {
		t.Fatal("different email must be a different user")
	}
	if a.Email != "ana@example.com" {
		t.Fatalf("email not normalized: %q", a.Email)
	}
	if _, err := s.UpsertUserForIdentity(ctx, Identity{Provider: "google", Subject: "3"}, t0); err == nil {
		t.Fatal("identity without email must be rejected")
	}
}

func TestWebSessionExpiry(t *testing.T) {
	s, ctx := newStore(t), context.Background()
	u, _ := s.UpsertUserForIdentity(ctx, Identity{Provider: "dev", Subject: "a@x", Email: "a@x"}, t0)
	_, hash := NewToken()
	if err := s.CreateWebSession(ctx, hash, u.ID, t0, t0.Add(time.Hour), "1.2.3.4", "ua"); err != nil {
		t.Fatal(err)
	}
	if got, err := s.UserForWebSession(ctx, hash, t0.Add(59*time.Minute)); err != nil || got.ID != u.ID {
		t.Fatalf("valid session: %v %v", got, err)
	}
	if _, err := s.UserForWebSession(ctx, hash, t0.Add(time.Hour)); !errors.Is(err, ErrNotFound) {
		t.Fatalf("expired session: err = %v", err)
	}
	_ = s.DeleteWebSession(ctx, hash)
	if _, err := s.UserForWebSession(ctx, hash, t0); !errors.Is(err, ErrNotFound) {
		t.Fatalf("deleted session: err = %v", err)
	}
}

func TestOAuthStateIsSingleUse(t *testing.T) {
	s, ctx := newStore(t), context.Background()
	st := OAuthState{State: "abc", Provider: "google", Verifier: "v", Nonce: "n", ReturnTo: "/", Expires: t0.Add(time.Minute)}
	if err := s.PutOAuthState(ctx, st); err != nil {
		t.Fatal(err)
	}
	if _, err := s.TakeOAuthState(ctx, "abc", t0); err != nil {
		t.Fatal(err)
	}
	if _, err := s.TakeOAuthState(ctx, "abc", t0); !errors.Is(err, ErrNotFound) {
		t.Fatalf("second take: err = %v", err)
	}
	_ = s.PutOAuthState(ctx, OAuthState{State: "old", Expires: t0})
	if _, err := s.TakeOAuthState(ctx, "old", t0); !errors.Is(err, ErrNotFound) {
		t.Fatalf("expired take: err = %v", err)
	}
}

func TestPairingApprovalCreatesDevice(t *testing.T) {
	s, ctx := newStore(t), context.Background()
	u, _ := s.UpsertUserForIdentity(ctx, Identity{Provider: "dev", Subject: "a@x", Email: "a@x"}, t0)
	code := HashToken("CODE")
	p := Pairing{ID: NewID("pair"), CodeHash: code, PublicKey: []byte("pk-1"), Name: "Mac", OS: "macos", CreatedAt: t0, ExpiresAt: t0.Add(10 * time.Minute)}
	if err := s.CreatePairing(ctx, p); err != nil {
		t.Fatal(err)
	}
	d, err := s.ApprovePairing(ctx, code, u.ID, t0.Add(time.Minute))
	if err != nil {
		t.Fatal(err)
	}
	got, err := s.GetPairing(ctx, p.ID, t0.Add(time.Minute))
	if err != nil || got.Status != "approved" || got.DeviceID != d.ID || got.UserID != u.ID {
		t.Fatalf("pairing after approve: %+v %v", got, err)
	}
	if _, err := s.ApprovePairing(ctx, code, u.ID, t0.Add(time.Minute)); !errors.Is(err, ErrNotFound) {
		t.Fatalf("double approve: err = %v", err)
	}
	list, _ := s.ListDevices(ctx, u.ID)
	if len(list) != 1 || list[0].Name != "Mac" {
		t.Fatalf("devices = %+v", list)
	}

	// Re-enrolling the same key is a conflict, not a second device.
	p2 := Pairing{ID: NewID("pair"), CodeHash: HashToken("CODE2"), PublicKey: []byte("pk-1"), Name: "Mac", OS: "macos", CreatedAt: t0, ExpiresAt: t0.Add(10 * time.Minute)}
	_ = s.CreatePairing(ctx, p2)
	if _, err := s.ApprovePairing(ctx, p2.CodeHash, u.ID, t0); !errors.Is(err, ErrConflict) {
		t.Fatalf("duplicate key: err = %v", err)
	}
}

func TestPairingExpires(t *testing.T) {
	s, ctx := newStore(t), context.Background()
	u, _ := s.UpsertUserForIdentity(ctx, Identity{Provider: "dev", Subject: "a@x", Email: "a@x"}, t0)
	p := Pairing{ID: NewID("pair"), CodeHash: HashToken("C"), PublicKey: []byte("pk"), Name: "n", OS: "o", CreatedAt: t0, ExpiresAt: t0.Add(10 * time.Minute)}
	_ = s.CreatePairing(ctx, p)
	if _, err := s.ApprovePairing(ctx, p.CodeHash, u.ID, t0.Add(10*time.Minute)); !errors.Is(err, ErrNotFound) {
		t.Fatalf("approve after expiry: err = %v", err)
	}
}

func TestDeviceTokens(t *testing.T) {
	s, ctx := newStore(t), context.Background()
	u, _ := s.UpsertUserForIdentity(ctx, Identity{Provider: "dev", Subject: "a@x", Email: "a@x"}, t0)
	_ = s.CreatePairing(ctx, Pairing{ID: "pair_1", CodeHash: HashToken("C"), PublicKey: []byte("pk"), Name: "n", OS: "o", CreatedAt: t0, ExpiresAt: t0.Add(time.Hour)})
	d, _ := s.ApprovePairing(ctx, HashToken("C"), u.ID, t0)

	_, hash := NewToken()
	_ = s.CreateDeviceToken(ctx, hash, d.ID, t0.Add(15*time.Minute))
	if got, err := s.DeviceForToken(ctx, hash, t0); err != nil || got.ID != d.ID || got.UserID != u.ID {
		t.Fatalf("token lookup: %+v %v", got, err)
	}
	if _, err := s.DeviceForToken(ctx, hash, t0.Add(15*time.Minute)); !errors.Is(err, ErrNotFound) {
		t.Fatalf("expired token: err = %v", err)
	}
	if err := s.TouchDevice(ctx, d.ID, t0.Add(time.Minute), "0.2.0", ""); err != nil {
		t.Fatal(err)
	}
	got, _ := s.GetDevice(ctx, d.ID)
	if got.AgentVersion != "0.2.0" || got.OS != "o" || !got.LastSeenAt.Equal(t0.Add(time.Minute)) {
		t.Fatalf("touch: %+v", got)
	}
}
