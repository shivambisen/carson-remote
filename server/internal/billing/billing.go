// Package billing sells lifetime access for a one-time payment through Razorpay (UPI, cards,
// netbanking). Without Razorpay keys it is disabled and everyone has access (self-hosting, dev).
//
// Flow: the signed-in page asks for an order (POST /v1/billing/order), opens Razorpay Checkout,
// and sends back the signed result (POST /v1/billing/verify). Razorpay's webhook
// (POST /billing/razorpay/webhook) records the same payment server-to-server, so access is
// granted even if the browser closes before verify.
package billing

import (
	"bytes"
	"context"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"time"

	"carson/server/internal/audit"
	"carson/server/internal/auth"
	"carson/server/internal/httpx"
	"carson/server/internal/ratelimit"
	"carson/server/internal/store"
)

const defaultAPI = "https://api.razorpay.com"

type Service struct {
	Store         *store.Store
	Auth          *auth.Service
	KeyID         string
	KeySecret     string
	WebhookSecret string
	Amount        int64 // smallest currency unit (paise): 9900 = ₹99
	Currency      string
	API           string // Razorpay API base (tests point it at a fake)
	HTTP          *http.Client
	Log           *slog.Logger
	Now           func() time.Time
	Audit         *audit.Log
	OrderLimiter  *ratelimit.Limiter
}

// Enabled reports whether payments are required at all.
func (s *Service) Enabled() bool { return s != nil && s.KeyID != "" && s.KeySecret != "" }

// HasAccess reports whether the user may add and connect to computers.
func (s *Service) HasAccess(ctx context.Context, userID string) (bool, error) {
	if !s.Enabled() {
		return true, nil
	}
	return s.Store.HasPaid(ctx, userID)
}

func (s *Service) Routes(mux *http.ServeMux) {
	mux.Handle("GET /v1/billing", s.Auth.RequireUser(http.HandlerFunc(s.handleStatus)))
	mux.Handle("POST /v1/billing/order", s.Auth.RequireUser(http.HandlerFunc(s.handleOrder)))
	mux.Handle("POST /v1/billing/verify", s.Auth.RequireUser(http.HandlerFunc(s.handleVerify)))
	mux.HandleFunc("POST /billing/razorpay/webhook", s.handleWebhook)
}

func (s *Service) handleStatus(w http.ResponseWriter, r *http.Request) {
	u, _ := auth.UserFrom(r.Context())
	paid, err := s.HasAccess(r.Context(), u.ID)
	if err != nil {
		s.internal(w, "billing status", err)
		return
	}
	httpx.WriteJSON(w, http.StatusOK, map[string]any{
		"enabled":  s.Enabled(),
		"paid":     paid,
		"amount":   s.Amount,
		"currency": s.Currency,
	})
}

func (s *Service) handleOrder(w http.ResponseWriter, r *http.Request) {
	u, _ := auth.UserFrom(r.Context())
	if !s.Enabled() {
		httpx.WriteError(w, http.StatusNotFound, "billing_disabled", "payments are not enabled on this server")
		return
	}
	if s.OrderLimiter != nil && !s.OrderLimiter.Allow(u.ID) {
		httpx.WriteError(w, http.StatusTooManyRequests, "rate_limited", "too many attempts; wait a minute")
		return
	}
	if paid, err := s.Store.HasPaid(r.Context(), u.ID); err != nil {
		s.internal(w, "billing order", err)
		return
	} else if paid {
		httpx.WriteError(w, http.StatusConflict, "already_paid", "this account already has glenley")
		return
	}
	id, err := s.createOrder(r.Context(), u.ID)
	if err != nil {
		s.Log.Error("razorpay create order", "err", err)
		httpx.WriteError(w, http.StatusBadGateway, "payment_provider", "could not start the payment; try again")
		return
	}
	if err := s.Store.CreatePurchase(r.Context(), store.Purchase{OrderID: id, UserID: u.ID, Amount: s.Amount, Currency: s.Currency, CreatedAt: s.Now()}); err != nil {
		s.internal(w, "save order", err)
		return
	}
	httpx.WriteJSON(w, http.StatusOK, map[string]any{
		"order_id": id, "key_id": s.KeyID, "amount": s.Amount, "currency": s.Currency,
		"email": u.Email, "name": u.DisplayName,
	})
}

// createOrder calls Razorpay's Orders API.
func (s *Service) createOrder(ctx context.Context, userID string) (string, error) {
	receipt := userID
	if len(receipt) > 40 { // Razorpay's receipt limit
		receipt = receipt[:40]
	}
	body, _ := json.Marshal(map[string]any{
		"amount": s.Amount, "currency": s.Currency, "receipt": receipt,
		"notes": map[string]string{"user_id": userID, "product": "glenley lifetime"},
	})
	api := s.API
	if api == "" {
		api = defaultAPI
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, api+"/v1/orders", bytes.NewReader(body))
	if err != nil {
		return "", err
	}
	req.SetBasicAuth(s.KeyID, s.KeySecret)
	req.Header.Set("Content-Type", "application/json")
	client := s.HTTP
	if client == nil {
		client = &http.Client{Timeout: 15 * time.Second}
	}
	resp, err := client.Do(req)
	if err != nil {
		return "", err
	}
	defer resp.Body.Close()
	raw, _ := io.ReadAll(io.LimitReader(resp.Body, 1<<16))
	if resp.StatusCode != http.StatusOK {
		return "", fmt.Errorf("razorpay orders: HTTP %d: %s", resp.StatusCode, raw)
	}
	var order struct {
		ID string `json:"id"`
	}
	if err := json.Unmarshal(raw, &order); err != nil || order.ID == "" {
		return "", fmt.Errorf("razorpay orders: unexpected response %s", raw)
	}
	return order.ID, nil
}

func (s *Service) handleVerify(w http.ResponseWriter, r *http.Request) {
	u, _ := auth.UserFrom(r.Context())
	var body struct {
		OrderID   string `json:"razorpay_order_id"`
		PaymentID string `json:"razorpay_payment_id"`
		Signature string `json:"razorpay_signature"`
	}
	if err := httpx.DecodeJSON(w, r, &body); err != nil || body.OrderID == "" || body.PaymentID == "" || body.Signature == "" {
		httpx.WriteError(w, http.StatusBadRequest, "bad_request", "order, payment and signature required")
		return
	}
	// Razorpay signs order_id|payment_id with the key secret.
	if !validHMAC(s.KeySecret, []byte(body.OrderID+"|"+body.PaymentID), body.Signature) {
		httpx.WriteError(w, http.StatusBadRequest, "bad_signature", "payment could not be verified")
		return
	}
	p, err := s.Store.GetPurchase(r.Context(), body.OrderID)
	if errors.Is(err, store.ErrNotFound) || (err == nil && p.UserID != u.ID) {
		httpx.WriteError(w, http.StatusNotFound, "unknown_order", "unknown order")
		return
	}
	if err != nil {
		s.internal(w, "get order", err)
		return
	}
	if err := s.markPaid(r.Context(), body.OrderID, body.PaymentID, httpx.ClientIP(r, s.Auth.TrustProxy), r.UserAgent()); err != nil {
		s.internal(w, "mark paid", err)
		return
	}
	httpx.WriteJSON(w, http.StatusOK, map[string]bool{"paid": true})
}

// handleWebhook records payments Razorpay reports (payment.captured, order.paid).
func (s *Service) handleWebhook(w http.ResponseWriter, r *http.Request) {
	raw, err := io.ReadAll(io.LimitReader(r.Body, 1<<20))
	if err != nil || s.WebhookSecret == "" || !validHMAC(s.WebhookSecret, raw, r.Header.Get("X-Razorpay-Signature")) {
		httpx.WriteError(w, http.StatusBadRequest, "bad_signature", "invalid signature")
		return
	}
	var ev struct {
		Event   string `json:"event"`
		Payload struct {
			Payment struct {
				Entity struct {
					ID       string `json:"id"`
					OrderID  string `json:"order_id"`
					Amount   int64  `json:"amount"`
					Currency string `json:"currency"`
				} `json:"entity"`
			} `json:"payment"`
		} `json:"payload"`
	}
	if err := json.Unmarshal(raw, &ev); err != nil {
		httpx.WriteError(w, http.StatusBadRequest, "bad_request", "invalid JSON")
		return
	}
	pay := ev.Payload.Payment.Entity
	if (ev.Event == "payment.captured" || ev.Event == "order.paid") && pay.OrderID != "" {
		p, err := s.Store.GetPurchase(r.Context(), pay.OrderID)
		switch {
		case errors.Is(err, store.ErrNotFound):
			s.Log.Warn("razorpay webhook for unknown order", "order_id", pay.OrderID)
		case err != nil:
			s.internal(w, "get order", err)
			return
		case pay.Amount < p.Amount || pay.Currency != p.Currency:
			s.Log.Warn("razorpay webhook amount mismatch", "order_id", pay.OrderID, "amount", pay.Amount)
		default:
			if err := s.markPaid(r.Context(), pay.OrderID, pay.ID, "", "razorpay webhook"); err != nil {
				s.internal(w, "mark paid", err)
				return
			}
		}
	}
	w.WriteHeader(http.StatusOK) // acknowledge, so Razorpay stops retrying
}

func (s *Service) markPaid(ctx context.Context, orderID, paymentID, ip, ua string) error {
	before, err := s.Store.GetPurchase(ctx, orderID)
	if err != nil {
		return err
	}
	p, err := s.Store.MarkPurchasePaid(ctx, orderID, paymentID, s.Now())
	if err != nil {
		return err
	}
	if before.Status != "paid" {
		s.Log.Info("purchase paid", "user_id", p.UserID, "order_id", orderID, "amount", p.Amount)
		if s.Audit != nil {
			s.Audit.Record(store.AuditEvent{UserID: p.UserID, Action: store.AuditPurchased, IP: ip, UserAgent: ua, Detail: fmt.Sprintf("%s %d.%02d", p.Currency, p.Amount/100, p.Amount%100)})
		}
	}
	return nil
}

func validHMAC(secret string, msg []byte, signatureHex string) bool {
	want, err := hex.DecodeString(signatureHex)
	if err != nil || secret == "" {
		return false
	}
	m := hmac.New(sha256.New, []byte(secret))
	m.Write(msg)
	return hmac.Equal(m.Sum(nil), want)
}

func (s *Service) internal(w http.ResponseWriter, msg string, err error) {
	s.Log.Error(msg, "err", err)
	httpx.WriteError(w, http.StatusInternalServerError, "internal", "internal error")
}
