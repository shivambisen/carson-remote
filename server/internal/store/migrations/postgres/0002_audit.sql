-- PostgreSQL version of ../0002_audit.sql (same tables, with BIGINT, BYTEA and BIGSERIAL types).
-- Append-only security log: who connected to which device, when, from where, and outcomes.
CREATE TABLE audit_log (
  id         BIGSERIAL PRIMARY KEY,
  at         BIGINT NOT NULL,
  user_id    TEXT NOT NULL,
  device_id  TEXT NOT NULL DEFAULT '',
  sid        TEXT NOT NULL DEFAULT '',
  action     TEXT NOT NULL,
  ip         TEXT NOT NULL DEFAULT '',
  user_agent TEXT NOT NULL DEFAULT '',
  detail     TEXT NOT NULL DEFAULT ''
);
CREATE INDEX audit_log_user_at ON audit_log (user_id, at DESC);
CREATE INDEX audit_log_at ON audit_log (at);
