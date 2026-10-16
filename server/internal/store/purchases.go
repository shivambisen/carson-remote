package store

import (
	"context"
	"database/sql"
	"errors"
	"time"
)

// Purchase is a one-time payment order (amount in the currency's smallest unit, e.g. paise).
type Purchase struct {
	OrderID   string
	UserID    string
	Amount    int64
	Currency  string
	Status    string // "created" or "paid"
	PaymentID string
	CreatedAt time.Time
	PaidAt    time.Time
}

func (s *Store) CreatePurchase(ctx context.Context, p Purchase) error {
	_, err := s.db.ExecContext(ctx, `INSERT INTO purchases (order_id, user_id, amount, currency, status, created_at) VALUES (?, ?, ?, ?, 'created', ?)`,
		p.OrderID, p.UserID, p.Amount, p.Currency, ms(p.CreatedAt))
	if isUniqueViolation(err) {
		return ErrConflict
	}
	return err
}

func (s *Store) GetPurchase(ctx context.Context, orderID string) (Purchase, error) {
	var p Purchase
	var created, paid int64
	err := s.db.QueryRowContext(ctx, `SELECT order_id, user_id, amount, currency, status, payment_id, created_at, paid_at FROM purchases WHERE order_id = ?`, orderID).
		Scan(&p.OrderID, &p.UserID, &p.Amount, &p.Currency, &p.Status, &p.PaymentID, &created, &paid)
	if errors.Is(err, sql.ErrNoRows) {
		return p, ErrNotFound
	}
	p.CreatedAt, p.PaidAt = fromMS(created), fromMS(paid)
	return p, err
}

// MarkPurchasePaid records the payment for an order. Idempotent: a repeated notification for the
// same order keeps the first payment. Returns the order (ErrNotFound if unknown).
func (s *Store) MarkPurchasePaid(ctx context.Context, orderID, paymentID string, now time.Time) (Purchase, error) {
	if _, err := s.db.ExecContext(ctx, `UPDATE purchases SET status = 'paid', payment_id = ?, paid_at = ? WHERE order_id = ? AND status = 'created'`,
		paymentID, ms(now), orderID); err != nil {
		return Purchase{}, err
	}
	return s.GetPurchase(ctx, orderID)
}

// HasPaid reports whether the user has any paid purchase.
func (s *Store) HasPaid(ctx context.Context, userID string) (bool, error) {
	var n int
	err := s.db.QueryRowContext(ctx, `SELECT COUNT(*) FROM purchases WHERE user_id = ? AND status = 'paid'`, userID).Scan(&n)
	return n > 0, err
}
