package auth

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"strconv"
	"sync"

	"github.com/coreos/go-oidc/v3/oidc"
	"golang.org/x/oauth2"

	"carson/server/internal/store"
)

// ErrUnverifiedEmail means the provider did not assert a verified email address.
var ErrUnverifiedEmail = errors.New("a verified email address is required")

// Provider is an external identity provider (OIDC or OAuth2).
type Provider interface {
	Name() string
	// AuthURL returns the provider's authorization URL for a PKCE + nonce login.
	AuthURL(ctx context.Context, state, verifier, nonce, redirectURL string) (string, error)
	// Exchange redeems the code and returns the verified identity.
	Exchange(ctx context.Context, code, verifier, nonce, redirectURL string) (store.Identity, error)
}

// OIDCProvider is a standards-based OpenID Connect provider (e.g. Google).
// Discovery runs lazily so the server starts even when the issuer is unreachable.
type OIDCProvider struct {
	name, issuer, clientID, clientSecret string

	mu       sync.Mutex
	provider *oidc.Provider
}

// NewOIDC returns an OIDC provider. Google's issuer is https://accounts.google.com.
func NewOIDC(name, issuer, clientID, clientSecret string) *OIDCProvider {
	return &OIDCProvider{name: name, issuer: issuer, clientID: clientID, clientSecret: clientSecret}
}

func (p *OIDCProvider) Name() string { return p.name }

func (p *OIDCProvider) discover(ctx context.Context) (*oidc.Provider, error) {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.provider != nil {
		return p.provider, nil
	}
	prov, err := oidc.NewProvider(ctx, p.issuer)
	if err != nil {
		return nil, fmt.Errorf("oidc discovery for %s: %w", p.issuer, err)
	}
	p.provider = prov
	return prov, nil
}

func (p *OIDCProvider) oauthConfig(prov *oidc.Provider, redirectURL string) *oauth2.Config {
	return &oauth2.Config{
		ClientID:     p.clientID,
		ClientSecret: p.clientSecret,
		Endpoint:     prov.Endpoint(),
		RedirectURL:  redirectURL,
		Scopes:       []string{oidc.ScopeOpenID, "email", "profile"},
	}
}

func (p *OIDCProvider) AuthURL(ctx context.Context, state, verifier, nonce, redirectURL string) (string, error) {
	prov, err := p.discover(ctx)
	if err != nil {
		return "", err
	}
	return p.oauthConfig(prov, redirectURL).AuthCodeURL(state, oauth2.S256ChallengeOption(verifier), oidc.Nonce(nonce)), nil
}

func (p *OIDCProvider) Exchange(ctx context.Context, code, verifier, nonce, redirectURL string) (store.Identity, error) {
	prov, err := p.discover(ctx)
	if err != nil {
		return store.Identity{}, err
	}
	tok, err := p.oauthConfig(prov, redirectURL).Exchange(ctx, code, oauth2.VerifierOption(verifier))
	if err != nil {
		return store.Identity{}, fmt.Errorf("token exchange: %w", err)
	}
	raw, ok := tok.Extra("id_token").(string)
	if !ok {
		return store.Identity{}, errors.New("token response has no id_token")
	}
	idTok, err := prov.Verifier(&oidc.Config{ClientID: p.clientID}).Verify(ctx, raw)
	if err != nil {
		return store.Identity{}, fmt.Errorf("verify id_token: %w", err)
	}
	if idTok.Nonce != nonce {
		return store.Identity{}, errors.New("id_token nonce mismatch")
	}
	var claims struct {
		Email         string   `json:"email"`
		EmailVerified flexBool `json:"email_verified"`
		Name          string   `json:"name"`
	}
	if err := idTok.Claims(&claims); err != nil {
		return store.Identity{}, err
	}
	if claims.Email == "" || !claims.EmailVerified {
		return store.Identity{}, ErrUnverifiedEmail
	}
	return store.Identity{Provider: p.name, Subject: idTok.Subject, Email: claims.Email, Name: claims.Name}, nil
}

// flexBool accepts both true and "true" (some providers send strings).
type flexBool bool

func (b *flexBool) UnmarshalJSON(data []byte) error {
	switch string(data) {
	case "true", `"true"`:
		*b = true
	case "false", `"false"`, "null":
		*b = false
	default:
		return fmt.Errorf("invalid boolean %s", data)
	}
	return nil
}

// GitHubProvider signs in with GitHub OAuth (GitHub does not offer OIDC for users).
type GitHubProvider struct {
	clientID, clientSecret string
	// Endpoints are overridable for tests.
	AuthEndpoint, TokenEndpoint, APIBase string
	HTTPClient                           *http.Client
}

func NewGitHub(clientID, clientSecret string) *GitHubProvider {
	return &GitHubProvider{
		clientID:      clientID,
		clientSecret:  clientSecret,
		AuthEndpoint:  "https://github.com/login/oauth/authorize",
		TokenEndpoint: "https://github.com/login/oauth/access_token",
		APIBase:       "https://api.github.com",
		HTTPClient:    http.DefaultClient,
	}
}

func (p *GitHubProvider) Name() string { return "github" }

func (p *GitHubProvider) oauthConfig(redirectURL string) *oauth2.Config {
	return &oauth2.Config{
		ClientID:     p.clientID,
		ClientSecret: p.clientSecret,
		Endpoint:     oauth2.Endpoint{AuthURL: p.AuthEndpoint, TokenURL: p.TokenEndpoint},
		RedirectURL:  redirectURL,
		Scopes:       []string{"read:user", "user:email"},
	}
}

func (p *GitHubProvider) AuthURL(_ context.Context, state, verifier, _, redirectURL string) (string, error) {
	return p.oauthConfig(redirectURL).AuthCodeURL(state, oauth2.S256ChallengeOption(verifier)), nil
}

func (p *GitHubProvider) Exchange(ctx context.Context, code, verifier, _, redirectURL string) (store.Identity, error) {
	ctx = context.WithValue(ctx, oauth2.HTTPClient, p.HTTPClient)
	tok, err := p.oauthConfig(redirectURL).Exchange(ctx, code, oauth2.VerifierOption(verifier))
	if err != nil {
		return store.Identity{}, fmt.Errorf("token exchange: %w", err)
	}
	client := oauth2.NewClient(ctx, oauth2.StaticTokenSource(tok))

	var user struct {
		ID    int64  `json:"id"`
		Login string `json:"login"`
		Name  string `json:"name"`
	}
	if err := getJSON(ctx, client, p.APIBase+"/user", &user); err != nil {
		return store.Identity{}, err
	}
	var emails []struct {
		Email    string `json:"email"`
		Primary  bool   `json:"primary"`
		Verified bool   `json:"verified"`
	}
	if err := getJSON(ctx, client, p.APIBase+"/user/emails", &emails); err != nil {
		return store.Identity{}, err
	}
	for _, e := range emails {
		if e.Primary && e.Verified {
			name := user.Name
			if name == "" {
				name = user.Login
			}
			return store.Identity{Provider: "github", Subject: strconv.FormatInt(user.ID, 10), Email: e.Email, Name: name}, nil
		}
	}
	return store.Identity{}, ErrUnverifiedEmail
}

func getJSON(ctx context.Context, client *http.Client, url string, dst any) error {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return err
	}
	req.Header.Set("Accept", "application/vnd.github+json")
	resp, err := client.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("GET %s: status %d", url, resp.StatusCode)
	}
	return json.NewDecoder(resp.Body).Decode(dst)
}
