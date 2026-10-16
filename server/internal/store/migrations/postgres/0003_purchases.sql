-- PostgreSQL version of ../0003_purchases.sql
-- One-time purchases (Razorpay orders). An account has access once any of its orders is paid.
CREATE TABLE purchases (
  order_id   TEXT PRIMARY KEY,
  user_id    TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  amount     BIGINT NOT NULL,
  currency   TEXT NOT NULL,
  status     TEXT NOT NULL CHECK (status IN ('created', 'paid')),
  payment_id TEXT NOT NULL DEFAULT '',
  created_at BIGINT NOT NULL,
  paid_at    BIGINT NOT NULL DEFAULT 0
);
CREATE INDEX purchases_user ON purchases (user_id, status);
