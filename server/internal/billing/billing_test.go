package billing

import (
	"bytes"
	"context"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"net/http/cookiejar"
	"net/http/httptest"
	"net/url"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"carson/server/internal/auth"
	"carson/server/internal/ratelimit"
	"carson/server/internal/store/storetest"
)

func sign(secret, msg string) string {
	m := hmac.New(sha256.New, []byte(secret))
	m.Write([]byte(msg))
	return hex.EncodeToString(m.Sum(nil))
}

type env struct {
	srv    *httptest.Server
	svc    *Service
	orders atomic.Int32
}

func newEnv(t *testing.T, enabled bool) *env {
	t.Helper()
	e := &env{}
	// Fake Razorpay Orders API.
	rzp := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		user, pass, _ := r.BasicAuth()
		if r.URL.Path != "/v1/orders" || user != "rzp_test_key" || pass != "key-secret" {
			http.Error(w, "bad", http.StatusUnauthorized)
			return
		}
		var body struct {
			Amount   int64  `json:"amount"`
			Currency string `json:"currency"`
		}
		_ = json.NewDecoder(r.Body).Decode(&body)
		if body.Amount != 9900 || body.Currency != "INR" {
			http.Error(w, "bad amount", http.StatusBadRequest)
			return
		}
		n := e.orders.Add(1)
		_ = json.NewEncoder(w).Encode(map[string]any{"id": "order_" + string(rune('A'+n)), "status": "created"})
	}))
	t.Cleanup(rzp.Close)

	st := storetest.Open(t)
	mux := http.NewServeMux()
	e.srv = httptest.NewServer(mux)
	t.Cleanup(e.srv.Close)
	pub, _ := url.Parse(e.srv.URL)
	log := slog.New(slog.NewTextHandler(io.Discard, nil))
	a := &auth.Service{Store: st, PublicURL: pub, Dev: true, Log: log, Now: time.Now, LoginLimiter: ratelimit.New(100, time.Minute, 100), Providers: map[string]auth.Provider{}}
	a.Routes(mux)
	e.svc = &Service{Store: st, Auth: a, Amount: 9900, Currency: "INR", API: rzp.URL, Log: log, Now: time.Now, WebhookSecret: "hook-secret"}
	if enabled {
		e.svc.KeyID, e.svc.KeySecret = "rzp_test_key", "key-secret"
	}
	e.svc.Routes(mux)
	return e
}

func (e *env) user(t *testing.T, email string) *http.Client {
	jar, _ := cookiejar.New(nil)
	c := &http.Client{Jar: jar}
	if code, _ := e.call(t, c, "POST", "/auth/dev/login", map[string]string{"email": email}, nil); code != 200 {
		t.Fatalf("login %d", code)
	}
	return c
}

func (e *env) call(t *testing.T, c *http.Client, method, path string, body, out any) (int, string) {
	t.Helper()
	b, _ := json.Marshal(body)
	req, _ := http.NewRequest(method, e.srv.URL+path, bytes.NewReader(b))
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Origin", e.srv.URL)
	if c == nil {
		c = http.DefaultClient
	}
	resp, err := c.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	raw, _ := io.ReadAll(resp.Body)
	if out != nil {
		_ = json.Unmarshal(raw, out)
	}
	return resp.StatusCode, string(raw)
}

type status struct {
	Enabled, Paid bool
	Amount        int64
}

func TestDisabledBillingGivesEveryoneAccess(t *testing.T) {
	e := newEnv(t, false)
	ana := e.user(t, "ana@x.com")
	var st status
	e.call(t, ana, "GET", "/v1/billing", nil, &st)
	if st.Enabled || !st.Paid {
		t.Fatalf("status = %+v", st)
	}
	if code, _ := e.call(t, ana, "POST", "/v1/billing/order", map[string]string{}, nil); code != http.StatusNotFound {
		t.Fatalf("order without billing = %d", code)
	}
}

func TestCheckoutVerifyGrantsLifetimeAccess(t *testing.T) {
	e := newEnv(t, true)
	ana, bo := e.user(t, "ana@x.com"), e.user(t, "bo@x.com")
	var st status
	e.call(t, ana, "GET", "/v1/billing", nil, &st)
	if !st.Enabled || st.Paid || st.Amount != 9900 {
		t.Fatalf("before = %+v", st)
	}
	var order struct {
		OrderID string `json:"order_id"`
		KeyID   string `json:"key_id"`
		Amount  int64
	}
	if code, body := e.call(t, ana, "POST", "/v1/billing/order", map[string]string{}, &order); code != 200 || order.OrderID == "" || order.KeyID != "rzp_test_key" {
		t.Fatalf("order = %d %s", code, body)
	}
	verify := func(c *http.Client, sig string) int {
		code, _ := e.call(t, c, "POST", "/v1/billing/verify", map[string]string{
			"razorpay_order_id": order.OrderID, "razorpay_payment_id": "pay_1", "razorpay_signature": sig,
		}, nil)
		return code
	}
	if verify(ana, sign("wrong", order.OrderID+"|pay_1")) != http.StatusBadRequest {
		t.Fatal("forged signature accepted")
	}
	good := sign("key-secret", order.OrderID+"|pay_1")
	if verify(bo, good) != http.StatusNotFound {
		t.Fatal("someone else's order must not unlock bo")
	}
	if verify(ana, good) != http.StatusOK {
		t.Fatal("valid payment rejected")
	}
	e.call(t, ana, "GET", "/v1/billing", nil, &st)
	if !st.Paid {
		t.Fatal("ana should have access")
	}
	if ok, _ := e.svc.HasAccess(context.Background(), "nobody"); ok {
		t.Fatal("unknown users have no access")
	}
	if code, _ := e.call(t, ana, "POST", "/v1/billing/order", map[string]string{}, nil); code != http.StatusConflict {
		t.Fatalf("second purchase = %d", code)
	}
}

func TestWebhookGrantsAccessWithoutTheBrowser(t *testing.T) {
	e := newEnv(t, true)
	ana := e.user(t, "ana@x.com")
	var order struct {
		OrderID string `json:"order_id"`
	}
	e.call(t, ana, "POST", "/v1/billing/order", map[string]string{}, &order)
	hook := func(body string, sig string) int {
		req, _ := http.NewRequest("POST", e.srv.URL+"/billing/razorpay/webhook", strings.NewReader(body))
		req.Header.Set("X-Razorpay-Signature", sig)
		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatal(err)
		}
		resp.Body.Close()
		return resp.StatusCode
	}
	event := func(amount int) string {
		b, _ := json.Marshal(map[string]any{"event": "payment.captured", "payload": map[string]any{"payment": map[string]any{"entity": map[string]any{
			"id": "pay_9", "order_id": order.OrderID, "amount": amount, "currency": "INR",
		}}}})
		return string(b)
	}
	if hook(event(9900), sign("wrong", event(9900))) != http.StatusBadRequest {
		t.Fatal("unsigned webhook accepted")
	}
	var st status
	if hook(event(100), sign("hook-secret", event(100))) != http.StatusOK {
		t.Fatal("webhook must be acknowledged")
	}
	if e.call(t, ana, "GET", "/v1/billing", nil, &st); st.Paid {
		t.Fatal("an underpayment must not grant access")
	}
	if hook(event(9900), sign("hook-secret", event(9900))) != http.StatusOK {
		t.Fatal("webhook rejected")
	}
	if e.call(t, ana, "GET", "/v1/billing", nil, &st); !st.Paid {
		t.Fatal("webhook should grant access")
	}
}
