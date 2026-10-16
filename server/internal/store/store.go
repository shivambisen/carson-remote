// Package store is carson-server's persistence layer: SQLite (pure-Go modernc driver) by
// default, or PostgreSQL (pgx) when given a postgres:// URL. Queries are written once with `?`
// placeholders and rewritten for Postgres. All queries are parameterized; callers pass time
// explicitly so tests control the clock.
package store

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"database/sql"
	"embed"
	"encoding/base32"
	"errors"
	"fmt"
	"io/fs"
	"net/url"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/stdlib" // also registers the "pgx" driver
	_ "modernc.org/sqlite"           // registers the "sqlite" driver
)

var (
	ErrNotFound = errors.New("not found")
	ErrConflict = errors.New("conflict")
)

//go:embed migrations/*.sql migrations/postgres/*.sql
var migrations embed.FS

// Store wraps the database handle.
type Store struct {
	db *DB
}

// DB and Tx are *sql.DB and *sql.Tx whose query methods accept `?` placeholders on every
// database.
type DB struct {
	*sql.DB
	pg bool
}

type Tx struct {
	*sql.Tx
	pg bool
}

func (d *DB) ExecContext(ctx context.Context, q string, args ...any) (sql.Result, error) {
	return d.DB.ExecContext(ctx, rebind(d.pg, q), args...)
}
func (d *DB) QueryContext(ctx context.Context, q string, args ...any) (*sql.Rows, error) {
	return d.DB.QueryContext(ctx, rebind(d.pg, q), args...)
}
func (d *DB) QueryRowContext(ctx context.Context, q string, args ...any) *sql.Row {
	return d.DB.QueryRowContext(ctx, rebind(d.pg, q), args...)
}
func (t *Tx) ExecContext(ctx context.Context, q string, args ...any) (sql.Result, error) {
	return t.Tx.ExecContext(ctx, rebind(t.pg, q), args...)
}
func (t *Tx) QueryContext(ctx context.Context, q string, args ...any) (*sql.Rows, error) {
	return t.Tx.QueryContext(ctx, rebind(t.pg, q), args...)
}
func (t *Tx) QueryRowContext(ctx context.Context, q string, args ...any) *sql.Row {
	return t.Tx.QueryRowContext(ctx, rebind(t.pg, q), args...)
}

// rebind turns `?` placeholders into Postgres's `$1, $2, …` (outside quoted literals).
func rebind(pg bool, q string) string {
	if !pg || !strings.Contains(q, "?") {
		return q
	}
	var b strings.Builder
	n, quoted := 0, false
	for _, r := range q {
		switch {
		case r == '\'':
			quoted = !quoted
			b.WriteRune(r)
		case r == '?' && !quoted:
			n++
			fmt.Fprintf(&b, "$%d", n)
		default:
			b.WriteRune(r)
		}
	}
	return b.String()
}

// IsPostgres reports whether dsn is a PostgreSQL URL rather than a SQLite file path.
func IsPostgres(dsn string) bool {
	return strings.HasPrefix(dsn, "postgres://") || strings.HasPrefix(dsn, "postgresql://")
}

// Open opens the database and applies pending migrations: a postgres:// URL, or the path of a
// SQLite file (created if needed).
func Open(ctx context.Context, dsn string) (*Store, error) {
	if IsPostgres(dsn) {
		return openPostgres(ctx, dsn)
	}
	path := dsn
	if dir := filepath.Dir(path); dir != "." {
		if err := os.MkdirAll(dir, 0o700); err != nil {
			return nil, fmt.Errorf("create db dir: %w", err)
		}
	}
	sqliteDSN := "file:" + path + "?_pragma=journal_mode(WAL)&_pragma=busy_timeout(5000)&_pragma=foreign_keys(1)&_pragma=synchronous(NORMAL)"
	db, err := sql.Open("sqlite", sqliteDSN)
	if err != nil {
		return nil, err
	}
	// One writer connection keeps SQLite free of SQLITE_BUSY under concurrent requests.
	// Plenty for a single node; revisit (or move to Postgres) when scaling out.
	db.SetMaxOpenConns(1)
	return finishOpen(ctx, &DB{DB: db})
}

func openPostgres(ctx context.Context, dsn string) (*Store, error) {
	return openPostgresSchema(ctx, dsn, "")
}

func openPostgresSchema(ctx context.Context, dsn, schema string) (*Store, error) {
	u, err := url.Parse(dsn)
	if err != nil {
		return nil, fmt.Errorf("database URL: %w", err)
	}
	q := u.Query()
	// One round trip per query and safe behind transaction poolers (Neon's -pooler, PgBouncer):
	// no server-side prepared statements.
	if q.Get("default_query_exec_mode") == "" {
		q.Set("default_query_exec_mode", "simple_protocol")
	}
	u.RawQuery = q.Encode()
	cfg, err := pgx.ParseConfig(u.String())
	if err != nil {
		return nil, fmt.Errorf("database URL: %w", err)
	}
	var opts []stdlib.OptionOpenDB
	if schema != "" {
		set := "SET search_path TO " + pgx.Identifier{schema}.Sanitize()
		opts = append(opts, stdlib.OptionAfterConnect(func(ctx context.Context, c *pgx.Conn) error {
			_, err := c.Exec(ctx, set)
			return err
		}))
	}
	db := stdlib.OpenDB(*cfg, opts...)
	if schema != "" {
		var got sql.NullString
		if err := db.QueryRowContext(ctx, `SELECT current_schema()`).Scan(&got); err != nil || got.String != schema {
			db.Close()
			return nil, fmt.Errorf("refusing to open: connections use schema %q, not %q (%v)", got.String, schema, err)
		}
	}
	db.SetMaxOpenConns(10)
	db.SetMaxIdleConns(5)
	db.SetConnMaxIdleTime(5 * time.Minute)
	return finishOpen(ctx, &DB{DB: db, pg: true})
}

func finishOpen(ctx context.Context, db *DB) (*Store, error) {
	s := &Store{db: db}
	if err := s.migrate(ctx); err != nil {
		db.Close()
		return nil, err
	}
	return s, nil
}

func (s *Store) Close() error { return s.db.Close() }

// Ping reports whether the database is reachable (used by /readyz).
func (s *Store) Ping(ctx context.Context) error { return s.db.PingContext(ctx) }

func (s *Store) migrate(ctx context.Context) error {
	if _, err := s.db.ExecContext(ctx, `CREATE TABLE IF NOT EXISTS schema_migrations (name TEXT PRIMARY KEY, applied_at BIGINT NOT NULL)`); err != nil {
		return fmt.Errorf("migrations table: %w", err)
	}
	pattern := "migrations/*.sql"
	if s.db.pg {
		pattern = "migrations/postgres/*.sql"
	}
	names, err := fs.Glob(migrations, pattern)
	if err != nil {
		return err
	}
	sort.Strings(names)
	for _, name := range names {
		var exists int
		if err := s.db.QueryRowContext(ctx, `SELECT COUNT(*) FROM schema_migrations WHERE name = ?`, name).Scan(&exists); err != nil {
			return err
		}
		if exists > 0 {
			continue
		}
		body, err := migrations.ReadFile(name)
		if err != nil {
			return err
		}
		err = s.tx(ctx, func(tx *Tx) error {
			// One statement at a time: drivers differ on multi-statement Exec.
			for _, stmt := range strings.Split(string(body), ";") {
				if strings.TrimSpace(stripComments(stmt)) == "" {
					continue
				}
				if _, err := tx.ExecContext(ctx, stmt); err != nil {
					return err
				}
			}
			_, err := tx.ExecContext(ctx, `INSERT INTO schema_migrations (name, applied_at) VALUES (?, ?)`, name, time.Now().UnixMilli())
			return err
		})
		if err != nil {
			return fmt.Errorf("migration %s: %w", name, err)
		}
	}
	return nil
}

func stripComments(stmt string) string {
	var lines []string
	for _, l := range strings.Split(stmt, "\n") {
		if !strings.HasPrefix(strings.TrimSpace(l), "--") {
			lines = append(lines, l)
		}
	}
	return strings.Join(lines, "\n")
}

func (s *Store) tx(ctx context.Context, fn func(*Tx) error) error {
	raw, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	tx := &Tx{Tx: raw, pg: s.db.pg}
	if err := fn(tx); err != nil {
		_ = tx.Rollback()
		return err
	}
	return tx.Commit()
}

var idEncoding = base32.StdEncoding.WithPadding(base32.NoPadding)

// NewID returns a random identifier such as "dev_k3x…" (128 bits of entropy).
func NewID(prefix string) string {
	b := make([]byte, 16)
	_, _ = rand.Read(b) // crypto/rand.Read never fails on supported platforms
	return prefix + "_" + strings.ToLower(idEncoding.EncodeToString(b))
}

// NewToken returns a random bearer secret (256 bits) and the hash to persist.
func NewToken() (token string, hash []byte) {
	b := make([]byte, 32)
	_, _ = rand.Read(b)
	token = strings.ToLower(idEncoding.EncodeToString(b))
	return token, HashToken(token)
}

// HashToken hashes a high-entropy secret for storage. A fast hash is fine because the
// input is random; it is never used for user-chosen passwords.
func HashToken(token string) []byte {
	sum := sha256.Sum256([]byte(token))
	return sum[:]
}

func ms(t time.Time) int64 { return t.UnixMilli() }

func fromMS(v int64) time.Time {
	if v == 0 {
		return time.Time{}
	}
	return time.UnixMilli(v).UTC()
}

func isUniqueViolation(err error) bool {
	if err == nil {
		return false
	}
	msg := err.Error()
	return strings.Contains(msg, "UNIQUE constraint failed") || // SQLite
		strings.Contains(msg, "SQLSTATE 23505") // Postgres unique_violation
}

// OpenInSchema opens a Postgres database with every connection confined to schema (which
// must exist), for tests that must not touch the database's real tables. The schema is set
// on each new connection and verified before any table is created.
func OpenInSchema(ctx context.Context, dsn, schema string) (*Store, error) {
	// Direct endpoint: a transaction pooler (Neon "-pooler" hosts, PgBouncer) does not keep a
	// connection's search_path.
	u, err := url.Parse(dsn)
	if err != nil {
		return nil, err
	}
	u.Host = strings.Replace(u.Host, "-pooler.", ".", 1)
	return openPostgresSchema(ctx, u.String(), schema)
}

// DB exposes the handle for tests in other packages that need to set up unusual rows.
func (s *Store) DB() *DB { return s.db }
