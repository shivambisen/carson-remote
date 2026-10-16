// Package storetest opens a throwaway store for tests: SQLite in a temp dir, or, when
// CARSON_TEST_DATABASE_URL is set, a fresh schema in that PostgreSQL database that is dropped
// afterwards (so tests can run against a real Postgres without touching its data).
package storetest

import (
	"context"
	"crypto/rand"
	"database/sql"
	"encoding/hex"
	"os"
	"path/filepath"
	"testing"

	"carson/server/internal/store"
)

func Open(t testing.TB) *store.Store {
	t.Helper()
	ctx := context.Background()
	base := os.Getenv("CARSON_TEST_DATABASE_URL")
	if base == "" {
		st, err := store.Open(ctx, filepath.Join(t.TempDir(), "carson.db"))
		if err != nil {
			t.Fatal(err)
		}
		t.Cleanup(func() { st.Close() })
		return st
	}
	b := make([]byte, 6)
	_, _ = rand.Read(b)
	schema := "carson_test_" + hex.EncodeToString(b)
	admin, err := sql.Open("pgx", base)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := admin.ExecContext(ctx, `CREATE SCHEMA `+schema); err != nil {
		admin.Close()
		t.Fatal(err)
	}
	// Registered first, so it runs last: after the store is closed, even if opening failed.
	t.Cleanup(func() {
		if _, err := admin.ExecContext(context.Background(), `DROP SCHEMA `+schema+` CASCADE`); err != nil {
			t.Errorf("drop test schema %s: %v", schema, err)
		}
		admin.Close()
	})
	st, err := store.OpenInSchema(ctx, base, schema)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { st.Close() })
	return st
}
