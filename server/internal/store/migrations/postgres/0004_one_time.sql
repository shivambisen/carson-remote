-- PostgreSQL version of ../0004_one_time.sql
-- Computers shared "just this once" with a code: removed from the account after one session.
ALTER TABLE pairings ADD COLUMN temporary INTEGER NOT NULL DEFAULT 0;
ALTER TABLE devices ADD COLUMN temporary INTEGER NOT NULL DEFAULT 0;
