// Package devices implements agent enrollment (pairing codes), device authentication
// (Ed25519 challenge → short-lived access token), and the "My devices" listing.
package devices

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"encoding/hex"
	"errors"
	"log/slog"
	"net/http"
	"strconv"
	"strings"
	"sync"
	"time"
	"unicode/utf8"

	"carson/server/internal/audit"
	"carson/server/internal/auth"
	"carson/server/internal/httpx"
	"carson/server/internal/ratelimit"
	"carson/server/internal/store"
)

const (
	PairingTTL      = 10 * time.Minute
	PollInterval    = 5 * time.Second
	ChallengeTTL    = 60 * time.Second
	AccessTokenTTL  = 15 * time.Minute
	maxNameLen      = 64
	maxChallenges   = 10_000
	codeAlphabet    = "BCDFGHJKLMNPQRSTVWXZ" // RFC 8628 §6.1: no vowels, no look-alikes
	codeLen         = 8
	authMessageHead = "carson-device-auth:v1\n"
)

var b64 = base64.RawURLEncoding

// AllowedOS lists the agent platforms we accept at enrollment.
var AllowedOS = map[string]bool{"macos": true, "windows": true, "linux": true}

// Presence reports live agent connections (implemented by the presence hub).
type Presence interface {
	Online(deviceID string) bool
}

// Limiters groups the rate limits this package enforces.
type Limiters struct {
	PairStart  *ratelimit.Limiter // per IP
	PairCode   *ratelimit.Limiter // per user and per IP: code lookups/approvals (anti-guessing)
	PairPoll   *ratelimit.Limiter // per IP
	DeviceAuth *ratelimit.Limiter // per IP
	Revoke     *ratelimit.Limiter // per user (nil = unlimited)
}

// Service wires the device endpoints.
type Service struct {
	Store      *store.Store
	Auth       *auth.Service
	Presence   Presence
	Limits     Limiters
	TrustProxy bool
	Log        *slog.Logger
	Now        func() time.Time
	Audit      *audit.Log
	// OnRevoke disconnects a revoked device's agent and ends its sessions (signaling hub).
	OnRevoke func(deviceID string)

	mu         sync.Mutex
	challenges map[string]challenge // nonce → challenge
	grants     map[string]grant     // hex(hash(grant)) → grant
}

// grant lets the agent that started a pairing finish it for the user who signed in in the
// browser. It is handed to the browser, which passes it to the agent over the agent's own
// loopback address (127.0.0.1), so only the computer that opened the browser can redeem it.
type grant struct {
	pairingID string
	userID    string
	ip, ua    string
	expires   time.Time
}

// GrantTTL bounds how long the browser → agent hand-off may take.
const GrantTTL = 2 * time.Minute

const maxGrants = 10000

type challenge struct {
	deviceID string
	expires  time.Time
}

func (s *Service) Routes(mux *http.ServeMux) {
	mux.HandleFunc("POST /v1/pair/start", s.handlePairStart)
	mux.HandleFunc("POST /v1/pair/poll", s.handlePairPoll)
	mux.Handle("POST /v1/pair/lookup", s.Auth.RequireUser(http.HandlerFunc(s.handlePairLookup)))
	mux.Handle("POST /v1/pair/approve", s.Auth.RequireUser(http.HandlerFunc(s.handlePairApprove)))
	// Browser sign-in (no code): see handlePairGrant.
	mux.Handle("POST /v1/pair/grant", s.Auth.RequireUser(http.HandlerFunc(s.handlePairGrant)))
	mux.HandleFunc("POST /v1/pair/complete", s.handlePairComplete)
	mux.HandleFunc("POST /v1/device/challenge", s.handleChallenge)
	mux.HandleFunc("POST /v1/device/token", s.handleToken)
	mux.Handle("GET /v1/devices", s.Auth.RequireUser(http.HandlerFunc(s.handleList)))
	mux.Handle("POST /v1/devices/{id}/revoke", s.Auth.RequireUser(http.HandlerFunc(s.handleRevoke)))
	mux.Handle("GET /v1/audit", s.Auth.RequireUser(http.HandlerFunc(s.handleAudit)))
}

// NewUserCode returns a random code like "BCDF-GHJK" (20^8 ≈ 2.6e10 possibilities).
func NewUserCode() string {
	var b strings.Builder
	buf := make([]byte, codeLen)
	_, _ = rand.Read(buf)
	for i, v := range buf {
		if i == codeLen/2 {
			b.WriteByte('-')
		}
		// 256 % 20 != 0 introduces a negligible bias; acceptable for a rate-limited, 10-minute code.
		b.WriteByte(codeAlphabet[int(v)%len(codeAlphabet)])
	}
	return b.String()
}

// NormalizeCode upper-cases and strips separators so "bcdf ghjk" matches "BCDF-GHJK".
func NormalizeCode(code string) string {
	var b strings.Builder
	for _, r := range strings.ToUpper(code) {
		if r != '-' && r != ' ' {
			b.WriteRune(r)
		}
	}
	return b.String()
}

func codeHash(code string) []byte { return store.HashToken("pair:" + NormalizeCode(code)) }

// AuthMessage is the exact byte string an agent signs to authenticate (see proto/device-auth.json).
func AuthMessage(deviceID, nonce string) []byte {
	return []byte(authMessageHead + deviceID + "\n" + nonce)
}

func (s *Service) ip(r *http.Request) string { return httpx.ClientIP(r, s.TrustProxy) }

func tooMany(w http.ResponseWriter) {
	httpx.WriteError(w, http.StatusTooManyRequests, "rate_limited", "too many requests, slow down")
}

func (s *Service) internal(w http.ResponseWriter, msg string, err error) {
	s.Log.Error(msg, "err", err)
	httpx.WriteError(w, http.StatusInternalServerError, "internal", "internal error")
}

func (s *Service) handlePairStart(w http.ResponseWriter, r *http.Request) {
	if !s.Limits.PairStart.Allow(s.ip(r)) {
		tooMany(w)
		return
	}
	var body struct {
		PublicKey string `json:"public_key"`
		Name      string `json:"name"`
		OS        string `json:"os"`
	}
	if err := httpx.DecodeJSON(w, r, &body); err != nil {
		httpx.WriteError(w, http.StatusBadRequest, "bad_request", err.Error())
		return
	}
	pk, err := b64.DecodeString(body.PublicKey)
	if err != nil || len(pk) != ed25519.PublicKeySize {
		httpx.WriteError(w, http.StatusBadRequest, "bad_public_key", "public_key must be a base64url Ed25519 key")
		return
	}
	name := strings.TrimSpace(body.Name)
	if name == "" || utf8.RuneCountInString(name) > maxNameLen || strings.ContainsFunc(name, isControl) {
		httpx.WriteError(w, http.StatusBadRequest, "bad_name", "name must be 1-64 printable characters")
		return
	}
	if !AllowedOS[body.OS] {
		httpx.WriteError(w, http.StatusBadRequest, "bad_os", "os must be macos, windows or linux")
		return
	}
	now := s.Now()
	for range 5 { // retry on the (astronomically unlikely) code collision
		code := NewUserCode()
		p := store.Pairing{ID: store.NewID("pair"), CodeHash: codeHash(code), PublicKey: pk, Name: name, OS: body.OS, CreatedAt: now, ExpiresAt: now.Add(PairingTTL)}
		err := s.Store.CreatePairing(r.Context(), p)
		if errors.Is(err, store.ErrConflict) {
			continue
		}
		if err != nil {
			s.internal(w, "create pairing", err)
			return
		}
		httpx.WriteJSON(w, http.StatusOK, map[string]any{
			"pairing_id":       p.ID,
			"user_code":        code,
			"verification_url": s.Auth.PublicURL.String() + "/#/pair",
			"expires_in":       int(PairingTTL.Seconds()),
			"interval":         int(PollInterval.Seconds()),
		})
		return
	}
	s.internal(w, "create pairing", errors.New("code collisions exhausted"))
}

func isControl(r rune) bool { return r < 0x20 || r == 0x7f }

func (s *Service) handlePairPoll(w http.ResponseWriter, r *http.Request) {
	if !s.Limits.PairPoll.Allow(s.ip(r)) {
		tooMany(w)
		return
	}
	var body struct {
		PairingID string `json:"pairing_id"`
	}
	if err := httpx.DecodeJSON(w, r, &body); err != nil {
		httpx.WriteError(w, http.StatusBadRequest, "bad_request", err.Error())
		return
	}
	p, err := s.Store.GetPairing(r.Context(), body.PairingID, s.Now())
	if errors.Is(err, store.ErrNotFound) {
		httpx.WriteError(w, http.StatusNotFound, "expired_token", "pairing expired or unknown; start again")
		return
	}
	if err != nil {
		s.internal(w, "get pairing", err)
		return
	}
	if p.Status != "approved" {
		httpx.WriteJSON(w, http.StatusOK, map[string]string{"status": "pending"})
		return
	}
	httpx.WriteJSON(w, http.StatusOK, map[string]string{"status": "approved", "device_id": p.DeviceID})
}

// codeRequest decodes {"code": …} for a signed-in user, applying anti-guessing limits.
func (s *Service) codeRequest(w http.ResponseWriter, r *http.Request) (store.User, string, bool) {
	u, _ := auth.UserFrom(r.Context())
	if !s.Limits.PairCode.Allow("user:"+u.ID) || !s.Limits.PairCode.Allow("ip:"+s.ip(r)) {
		tooMany(w)
		return u, "", false
	}
	var body struct {
		Code string `json:"code"`
	}
	if err := httpx.DecodeJSON(w, r, &body); err != nil {
		httpx.WriteError(w, http.StatusBadRequest, "bad_request", err.Error())
		return u, "", false
	}
	if len(NormalizeCode(body.Code)) != codeLen {
		httpx.WriteError(w, http.StatusBadRequest, "bad_code", "code must be 8 letters")
		return u, "", false
	}
	return u, body.Code, true
}

func (s *Service) handlePairLookup(w http.ResponseWriter, r *http.Request) {
	_, code, ok := s.codeRequest(w, r)
	if !ok {
		return
	}
	p, err := s.Store.PendingPairingByCode(r.Context(), codeHash(code), s.Now())
	if errors.Is(err, store.ErrNotFound) {
		httpx.WriteError(w, http.StatusNotFound, "invalid_code", "code is invalid or expired")
		return
	}
	if err != nil {
		s.internal(w, "lookup pairing", err)
		return
	}
	httpx.WriteJSON(w, http.StatusOK, map[string]any{"name": p.Name, "os": p.OS, "expires_at": p.ExpiresAt})
}

func (s *Service) handlePairApprove(w http.ResponseWriter, r *http.Request) {
	u, code, ok := s.codeRequest(w, r)
	if !ok {
		return
	}
	d, err := s.Store.ApprovePairing(r.Context(), codeHash(code), u.ID, s.Now())
	switch {
	case errors.Is(err, store.ErrNotFound):
		httpx.WriteError(w, http.StatusNotFound, "invalid_code", "code is invalid or expired")
	case errors.Is(err, store.ErrConflict):
		httpx.WriteError(w, http.StatusConflict, "already_paired", "this device is already paired; unpair it first")
	case err != nil:
		s.internal(w, "approve pairing", err)
	default:
		s.Log.Info("device paired", "user_id", u.ID, "device_id", d.ID, "os", d.OS)
		s.Audit.Record(store.AuditEvent{UserID: u.ID, DeviceID: d.ID, Action: store.AuditDevicePaired, IP: s.ip(r), UserAgent: r.UserAgent(), Detail: d.Name})
		httpx.WriteJSON(w, http.StatusOK, d)
	}
}

// handlePairGrant is called by the signed-in browser page that the agent opened. It does not
// approve anything by itself: it returns a one-time grant that the page forwards to the agent
// on 127.0.0.1, and only the agent presenting it (handlePairComplete) creates the device.
// A link to someone else's pairing is therefore useless: the grant would go to the victim's
// own loopback, where no agent is waiting for it.
func (s *Service) handlePairGrant(w http.ResponseWriter, r *http.Request) {
	u, _ := auth.UserFrom(r.Context())
	if !s.Limits.PairCode.Allow("user:"+u.ID) || !s.Limits.PairCode.Allow("ip:"+s.ip(r)) {
		tooMany(w)
		return
	}
	var body struct {
		PairingID string `json:"pairing_id"`
	}
	if err := httpx.DecodeJSON(w, r, &body); err != nil || body.PairingID == "" {
		httpx.WriteError(w, http.StatusBadRequest, "bad_request", "pairing_id required")
		return
	}
	now := s.Now()
	p, err := s.Store.GetPairing(r.Context(), body.PairingID, now)
	if errors.Is(err, store.ErrNotFound) || (err == nil && p.Status != "pending") {
		httpx.WriteError(w, http.StatusNotFound, "invalid_pairing", "this sign-in link expired; start again from the app")
		return
	}
	if err != nil {
		s.internal(w, "get pairing", err)
		return
	}
	token, hash := store.NewToken()
	s.mu.Lock()
	if s.grants == nil {
		s.grants = map[string]grant{}
	}
	for k, g := range s.grants {
		if now.After(g.expires) {
			delete(s.grants, k)
		}
	}
	full := len(s.grants) >= maxGrants
	if !full {
		s.grants[hex.EncodeToString(hash)] = grant{pairingID: p.ID, userID: u.ID, ip: s.ip(r), ua: r.UserAgent(), expires: now.Add(GrantTTL)}
	}
	s.mu.Unlock()
	if full {
		tooMany(w)
		return
	}
	httpx.WriteJSON(w, http.StatusOK, map[string]any{"grant": token, "name": p.Name, "os": p.OS})
}

// handlePairComplete is called by the agent with the grant it received on its loopback.
func (s *Service) handlePairComplete(w http.ResponseWriter, r *http.Request) {
	if !s.Limits.PairPoll.Allow(s.ip(r)) {
		tooMany(w)
		return
	}
	var body struct {
		PairingID string `json:"pairing_id"`
		Grant     string `json:"grant"`
	}
	if err := httpx.DecodeJSON(w, r, &body); err != nil || body.PairingID == "" || body.Grant == "" {
		httpx.WriteError(w, http.StatusBadRequest, "bad_request", "pairing_id and grant required")
		return
	}
	key := hex.EncodeToString(store.HashToken(body.Grant))
	now := s.Now()
	s.mu.Lock()
	g, ok := s.grants[key]
	delete(s.grants, key) // single use, even on failure
	s.mu.Unlock()
	if !ok || now.After(g.expires) || g.pairingID != body.PairingID {
		httpx.WriteError(w, http.StatusForbidden, "invalid_grant", "sign-in expired or invalid; start again")
		return
	}
	d, err := s.Store.ApprovePairingByID(r.Context(), g.pairingID, g.userID, now)
	switch {
	case errors.Is(err, store.ErrNotFound):
		httpx.WriteError(w, http.StatusNotFound, "expired_token", "pairing expired or already completed; start again")
	case errors.Is(err, store.ErrConflict):
		httpx.WriteError(w, http.StatusConflict, "already_paired", "this device is already paired; unpair it first")
	case err != nil:
		s.internal(w, "approve pairing", err)
	default:
		s.Log.Info("device paired", "user_id", g.userID, "device_id", d.ID, "os", d.OS, "via", "browser")
		s.Audit.Record(store.AuditEvent{UserID: g.userID, DeviceID: d.ID, Action: store.AuditDevicePaired, IP: g.ip, UserAgent: g.ua, Detail: d.Name})
		httpx.WriteJSON(w, http.StatusOK, map[string]string{"status": "approved", "device_id": d.ID})
	}
}

func (s *Service) handleChallenge(w http.ResponseWriter, r *http.Request) {
	if !s.Limits.DeviceAuth.Allow(s.ip(r)) {
		tooMany(w)
		return
	}
	var body struct {
		DeviceID string `json:"device_id"`
	}
	if err := httpx.DecodeJSON(w, r, &body); err != nil || body.DeviceID == "" {
		httpx.WriteError(w, http.StatusBadRequest, "bad_request", "device_id required")
		return
	}
	// Issued for any device_id so the endpoint does not reveal which devices exist.
	nonce, _ := store.NewToken()
	now := s.Now()
	s.mu.Lock()
	if s.challenges == nil {
		s.challenges = map[string]challenge{}
	}
	if len(s.challenges) >= maxChallenges {
		for k, c := range s.challenges {
			if !now.Before(c.expires) {
				delete(s.challenges, k)
			}
		}
	}
	full := len(s.challenges) >= maxChallenges
	if !full {
		s.challenges[nonce] = challenge{deviceID: body.DeviceID, expires: now.Add(ChallengeTTL)}
	}
	s.mu.Unlock()
	if full {
		httpx.WriteError(w, http.StatusServiceUnavailable, "busy", "try again shortly")
		return
	}
	httpx.WriteJSON(w, http.StatusOK, map[string]any{"nonce": nonce, "expires_in": int(ChallengeTTL.Seconds())})
}

func (s *Service) handleToken(w http.ResponseWriter, r *http.Request) {
	if !s.Limits.DeviceAuth.Allow(s.ip(r)) {
		tooMany(w)
		return
	}
	var body struct {
		DeviceID  string `json:"device_id"`
		Nonce     string `json:"nonce"`
		Signature string `json:"signature"`
	}
	if err := httpx.DecodeJSON(w, r, &body); err != nil {
		httpx.WriteError(w, http.StatusBadRequest, "bad_request", err.Error())
		return
	}
	now := s.Now()
	s.mu.Lock()
	c, ok := s.challenges[body.Nonce]
	delete(s.challenges, body.Nonce) // single use, even on failure
	s.mu.Unlock()

	deny := func() {
		httpx.WriteError(w, http.StatusUnauthorized, "invalid_grant", "device authentication failed")
	}
	if !ok || c.deviceID != body.DeviceID || !now.Before(c.expires) {
		deny()
		return
	}
	sig, err := b64.DecodeString(body.Signature)
	if err != nil || len(sig) != ed25519.SignatureSize {
		deny()
		return
	}
	d, err := s.Store.GetDevice(r.Context(), body.DeviceID)
	if errors.Is(err, store.ErrNotFound) || (err == nil && d.RevokedAt != nil) {
		deny()
		return
	}
	if err != nil {
		s.internal(w, "get device", err)
		return
	}
	if !ed25519.Verify(ed25519.PublicKey(d.PublicKey), AuthMessage(d.ID, body.Nonce), sig) {
		s.Log.Warn("device signature rejected", "device_id", d.ID)
		deny()
		return
	}
	token, hash := store.NewToken()
	if err := s.Store.CreateDeviceToken(r.Context(), hash, d.ID, now.Add(AccessTokenTTL)); err != nil {
		s.internal(w, "create device token", err)
		return
	}
	httpx.WriteJSON(w, http.StatusOK, map[string]any{"access_token": token, "token_type": "Bearer", "expires_in": int(AccessTokenTTL.Seconds())})
}

// DeviceFromRequest authenticates an agent by its "Authorization: Bearer <token>" header.
func (s *Service) DeviceFromRequest(ctx context.Context, r *http.Request) (store.Device, error) {
	token, ok := strings.CutPrefix(r.Header.Get("Authorization"), "Bearer ")
	if !ok || token == "" {
		return store.Device{}, store.ErrNotFound
	}
	return s.Store.DeviceForToken(ctx, store.HashToken(token), s.Now())
}

// DeviceView is a device as shown to its owner. PublicKey lets the viewer verify the agent's
// signed DTLS fingerprint (end-to-end identity, see docs §7).
type DeviceView struct {
	store.Device
	Online    bool   `json:"online"`
	PublicKey string `json:"public_key"`
}

// View builds the owner's view of a device.
func View(d store.Device, online bool) DeviceView {
	return DeviceView{Device: d, Online: online, PublicKey: b64.EncodeToString(d.PublicKey)}
}

func (s *Service) handleRevoke(w http.ResponseWriter, r *http.Request) {
	u, _ := auth.UserFrom(r.Context())
	if s.Limits.Revoke != nil && !s.Limits.Revoke.Allow(u.ID) {
		tooMany(w)
		return
	}
	id := r.PathValue("id")
	err := s.Store.RevokeDevice(r.Context(), u.ID, id, s.Now())
	if errors.Is(err, store.ErrNotFound) {
		httpx.WriteError(w, http.StatusNotFound, "not_found", "no such device on your account")
		return
	}
	if err != nil {
		s.internal(w, "revoke device", err)
		return
	}
	s.Log.Info("device revoked", "user_id", u.ID, "device_id", id)
	s.Audit.Record(store.AuditEvent{UserID: u.ID, DeviceID: id, Action: store.AuditDeviceRevoked, IP: s.ip(r), UserAgent: r.UserAgent()})
	if s.OnRevoke != nil {
		s.OnRevoke(id)
	}
	w.WriteHeader(http.StatusNoContent)
}

func (s *Service) handleAudit(w http.ResponseWriter, r *http.Request) {
	u, _ := auth.UserFrom(r.Context())
	before, _ := strconv.ParseInt(r.URL.Query().Get("before"), 10, 64)
	limit, _ := strconv.Atoi(r.URL.Query().Get("limit"))
	events, err := s.Store.ListAudit(r.Context(), u.ID, before, limit)
	if err != nil {
		s.internal(w, "list audit", err)
		return
	}
	httpx.WriteJSON(w, http.StatusOK, map[string]any{"events": events})
}

func (s *Service) handleList(w http.ResponseWriter, r *http.Request) {
	u, _ := auth.UserFrom(r.Context())
	list, err := s.Store.ListDevices(r.Context(), u.ID)
	if err != nil {
		s.internal(w, "list devices", err)
		return
	}
	views := make([]DeviceView, len(list))
	for i, d := range list {
		views[i] = View(d, s.Presence.Online(d.ID))
	}
	httpx.WriteJSON(w, http.StatusOK, map[string]any{"devices": views})
}
