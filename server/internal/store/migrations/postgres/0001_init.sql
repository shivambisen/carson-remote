-- PostgreSQL version of ../0001_init.sql (same tables, with BIGINT, BYTEA and BIGSERIAL types).
-- Timestamps are unix milliseconds (BIGINT). IDs are prefixed random strings.

CREATE TABLE users (
  id           TEXT PRIMARY KEY,
  email        TEXT NOT NULL,
  display_name TEXT NOT NULL DEFAULT '',
  created_at   BIGINT NOT NULL
);
CREATE UNIQUE INDEX users_email ON users (email);

-- One row per external login (google:<sub>, github:<id>, dev:<email>).
CREATE TABLE identities (
  provider   TEXT NOT NULL,
  subject    TEXT NOT NULL,
  user_id    TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  email      TEXT NOT NULL,
  created_at BIGINT NOT NULL,
  PRIMARY KEY (provider, subject)
);

-- Browser sessions. Only a SHA-256 of the cookie value is stored.
CREATE TABLE web_sessions (
  token_hash BYTEA PRIMARY KEY,
  user_id    TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  created_at BIGINT NOT NULL,
  expires_at BIGINT NOT NULL,
  ip         TEXT NOT NULL DEFAULT '',
  user_agent TEXT NOT NULL DEFAULT ''
);
CREATE INDEX web_sessions_user ON web_sessions (user_id);

-- In-flight OAuth/OIDC logins (state, PKCE verifier, nonce).
CREATE TABLE oauth_states (
  state      TEXT PRIMARY KEY,
  provider   TEXT NOT NULL,
  verifier   TEXT NOT NULL,
  nonce      TEXT NOT NULL,
  return_to  TEXT NOT NULL,
  expires_at BIGINT NOT NULL
);

CREATE TABLE devices (
  id            TEXT PRIMARY KEY,
  user_id       TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  name          TEXT NOT NULL,
  os            TEXT NOT NULL,
  public_key    BYTEA NOT NULL UNIQUE,
  agent_version TEXT NOT NULL DEFAULT '',
  created_at    BIGINT NOT NULL,
  last_seen_at  BIGINT NOT NULL DEFAULT 0,
  revoked_at    BIGINT
);
CREATE INDEX devices_user ON devices (user_id);

-- Device enrollment (RFC 8628-style). user_code is stored hashed.
CREATE TABLE pairings (
  id          TEXT PRIMARY KEY,
  code_hash   BYTEA NOT NULL UNIQUE,
  public_key  BYTEA NOT NULL,
  name        TEXT NOT NULL,
  os          TEXT NOT NULL,
  status      TEXT NOT NULL CHECK (status IN ('pending', 'approved')),
  user_id     TEXT REFERENCES users (id) ON DELETE CASCADE,
  device_id   TEXT REFERENCES devices (id) ON DELETE CASCADE,
  created_at  BIGINT NOT NULL,
  expires_at  BIGINT NOT NULL
);

-- Short-lived device access tokens (hashed).
CREATE TABLE device_tokens (
  token_hash BYTEA PRIMARY KEY,
  device_id  TEXT NOT NULL REFERENCES devices (id) ON DELETE CASCADE,
  expires_at BIGINT NOT NULL
);
