# carson — Design Doc (v0.1, draft for approval)

**Status:** Proposed · **Date:** 2026-10-15 · **Scope:** Milestones 1–8

carson lets a signed-in user see their own devices, pick one, and view and control its screen
with low latency. The building blocks are a host agent (`carson-agent`), a browser viewer
(`carson-viewer`), and a small signaling and control server (`carson-server`).

---

## 1. Decisions so far

| Topic | Decision | Why |
|---|---|---|
| Existing platform | None. Greenfield. carson ships its own accounts. | No existing repo, auth, or DB to reuse. |
| `carson-server` | **Go** | Cheap long-lived WebSockets. Ships as a single static binary. |
| `carson-agent` | **Rust** | Mature native capture/encode/input bindings. Safe FFI. No GC pauses while handling frames. |
| `carson-viewer` | **TypeScript + Vite**, no UI framework at first | The browser WebRTC API is enough. Fewer dependencies. Served as static files by the server. |
| Database | **SQLite (WAL)**, pure-Go driver, schema kept Postgres-compatible | Free. No extra process. Plenty for one node. |
| Presence | **In-memory** on one node, behind an interface. Add Redis only when scaling out. | Leaving Redis out keeps v1 free and simple (differs from the suggested stack on purpose). |
| Login | **OIDC sign-in with Google and GitHub**, plus a dev-only email login | Free. carson never stores passwords. |
| Hosting | **$0 path:** one free VM running server + coturn + Caddy (see §3) | You asked for free. |

## 2. "Can it be client ↔ host with no server?"

**Mostly yes. The heavy part already is.** Video, input, and clipboard travel **directly peer-to-peer**
over WebRTC and are encrypted end to end. They never pass through our server. A small server is
still needed for four things:

1. **Discovery.** "Show me all my devices and which are online" needs a shared place both laptops can reach.
2. **Signaling.** Two peers behind home routers can't find each other alone. They must swap a few KB of
   connection offers (SDP/ICE) once per session. That is roughly 5 KB per connection, not a video stream.
3. **Security.** Account checks, device revocation, and audit logs need a trusted authority. Without one,
   anyone who learns a device address could try to connect.
4. **Relay (TURN), as a fallback only.** About 10–20% of networks (corporate, mobile CGNAT) block direct
   P2P. Then traffic goes through a TURN relay, which only ever sees encrypted packets.

A truly serverless mode is possible, but only on the same LAN (mDNS discovery) or by copying codes by
hand. This doc puts it under future work. The server is tiny and runs free:

## 3. Hosting at $0

| Piece | Free option | Notes |
|---|---|---|
| Server + coturn | **Oracle Cloud "Always Free"** ARM VM (public IPv4, generous egress) | One `docker compose up`. Also works on any $4–5/mo VPS. |
| TLS | **Caddy** with Let's Encrypt | Automatic HTTPS. |
| Domain | Free subdomain (e.g. DuckDNS), or about $10/yr for your own | WebRTC and OAuth need HTTPS on a stable hostname. |
| STUN | Our coturn, plus public STUN as a backup | |
| TURN alternative | Cloudflare Realtime TURN free tier (check current limits) | Skips running coturn. Configured as a pluggable provider. |
| Local dev | Everything on the laptop via `docker compose` | Two devices on the same LAN work with no cloud at all. |

**Costs that are not free (flagged early):** shipping signed installers needs an **Apple Developer
account ($99/yr)** for macOS notarization and a **Windows code-signing certificate**. Without them, users get
Gatekeeper and SmartScreen warnings. That's fine for internal testing, not for public release.

## 4. Architecture

```
   Laptop A (viewer)                        carson-server (Go)                     Laptop B (host)
 ┌──────────────────┐   HTTPS + WSS   ┌───────────────────────────┐   WSS    ┌─────────────────────────┐
 │ carson-viewer    │◄──────────────►│ auth (OIDC) · sessions     │◄────────►│ carson-agent (Rust)     │
 │ (browser, TS)    │  signaling,    │ device registry · presence │ presence │  capture → encode → RTP │
 │  <video> + input │  device list   │ signaling relay · TURN     │ signaling│  input inject · clipboard│
 └────────┬─────────┘                │ creds · audit · ratelimit  │          │  tray · consent banner  │
          │                          │ SQLite                     │          └───────────┬─────────────┘
          │                          └───────────────────────────┘                      │
          │            WebRTC: DTLS-SRTP video + SCTP data channels (E2E)               │
          └──────────────── direct P2P (preferred) ──── or ──── coturn relay ───────────┘
```

## 5. Identity, enrollment, and authorization

- **User sign-in (viewer).** OIDC Authorization Code + PKCE with Google or GitHub. The server sets an
  opaque, httpOnly, `Secure`, `SameSite=Lax` session cookie. Session rows live in the DB so they can be revoked.
  WebSocket upgrades check the `Origin` header and the cookie.
- **Device enrollment (agent), like the OAuth device flow (RFC 8628).**
  1. The agent generates an **Ed25519 keypair**. The private key goes into the OS keychain (macOS Keychain,
     Windows DPAPI / Credential Manager, Linux Secret Service).
  2. The agent calls `POST /v1/pair/start` with its public key and gets back an 8-character code (10-minute TTL).
  3. The signed-in user enters the code at `/pair`, which binds the device to `user_id`.
  4. To authenticate, the agent signs a server nonce and receives a short-lived (15-minute) device access token.
     No long-lived bearer secret ever leaves the device.
- **Authorization runs on every request and every relayed signaling message.**
  `viewer.user_id == target_device.user_id`, the device is not revoked, and the session state allows the message.
  All of this is in one `authz` package with table-driven tests.
- **Revoke device:** delete the public key, close its WebSocket, end its active sessions, write an audit entry.

## 6. Signaling protocol (WSS `/v1/ws`, JSON)

Envelope: `{ "v":1, "type":"…", "id":"<msg uuid>", "sid":"<session id>?", "body":{…} }`

| Type | Direction | Purpose |
|---|---|---|
| `hello` / `hello.ok` | client→server | Identify as viewer or agent. Server returns the device list or config. |
| `presence` | server→viewer | A device went online or offline (agent heartbeat every 15 s, offline after 45 s). |
| `session.request` | viewer→server→agent | Ask to connect. The server checks authz and rate limits, then allocates `sid`. |
| `session.accept` / `reject` | agent→viewer | Consent result (prompt, unattended, or PIN pending). |
| `sdp.offer` / `sdp.answer` | both | The agent sends the offer because it owns the media track and creates the data channels. |
| `ice.candidate` | both | Trickle ICE. |
| `session.ice_restart` | either | Network changed. Renegotiate with `iceRestart`. |
| `session.resume` | either | Rejoin `sid` with a resume token within 30 s. |
| `session.end` | either | Clean teardown plus an audit entry. |
| `turn.creds` | server→both | Time-limited coturn credentials, `username=<expiry>:<sid>`, HMAC-SHA1 (coturn REST scheme), TTL 10 min. |

The server forwards SDP/ICE blobs without reading them. It does validate size limits (64 KB) and the
session state machine:
`requested → consenting → negotiating → connected ⇄ reconnecting → ended`.

## 7. Transport and encryption

- **Media:** one video track, DTLS-SRTP. **Data channels:**
  - `ptr`: `ordered:false, maxRetransmits:0`, for mouse moves (the newest position wins).
  - `ctl`: reliable and ordered, for keys, buttons, scroll, clipboard, monitor switching, PIN, and stats feedback.
- **End to end:** DTLS keys exist only on the two peers. TURN relays forward ciphertext.
  **Honest threat model:** as in Chrome Remote Desktop, the server is trusted to vouch for who a device is.
  A compromised server could in theory sit in the middle of a new session's DTLS fingerprints.
  Mitigation (M6): the agent signs its DTLS fingerprint with its device key, and the viewer checks it
  against a pinned key (trust-on-first-use, with a warning on change). The optional PIN is checked by the
  agent over the encrypted channel, and the server never sees it.
- **ICE:** host, srflx, and relay candidates. Direct paths are preferred, and relay is used only when ICE picks it.

## 8. Input protocol (binary, little-endian)

| Msg | Layout | Channel |
|---|---|---|
| PointerMove | `u8 type, u8 monitor, u16 x, u16 y` (0–65535, normalized) | ptr |
| Button | `u8 type, u8 button, u8 down` | ctl |
| Wheel | `u8 type, i16 dx, i16 dy` (pixel-mode deltas) | ctl |
| Key | `u8 type, u8 down, u16 mods, u8 len, utf8 KeyboardEvent.code` | ctl |
| Clipboard | `u8 type, u32 len, utf8 text` (max 1 MiB, chunked) | ctl |

Keys use the physical-key `code`, mapped on the agent to native scancodes, so layouts behave. The agent
**releases all held keys and buttons on disconnect**, so no key gets stuck down.

## 9. Host media pipeline (`carson-agent`)

```
capture (dirty rects) → GPU color convert BGRA→NV12 → HW H.264 encoder → RTP packetize → webrtc-rs
    ▲                                            ▲
    └── fps / region gating          bitrate & resolution from congestion controller + viewer feedback
```

| OS | Capture | HW encode | Input |
|---|---|---|---|
| macOS | ScreenCaptureKit | VideoToolbox | CGEvent (needs Accessibility) |
| Windows | DXGI Desktop Duplication (WGC fallback) | Media Foundation (NVENC/QSV/AMF) | SendInput |
| Linux (2nd) | PipeWire portal / X11 (XShm + XDamage) | VA-API | uinput / XTest |

- Software fallback: **OpenH264** (Cisco binary, royalty-covered), then VP8 (libvpx).
- Low-latency encoder settings: no B-frames, 1-frame VBV, periodic intra-refresh (no large keyframes),
  and keyframe on request (PLI).
- **Adaptive quality ladder:** bitrate first, then fps (60→30→15), then resolution (100%→75%→50%).
  Each step needs about 2 s of stable signal before stepping back up.
- Self-heal: capture and encode run in a supervised task. On error or a stall over 2 s, rebuild the pipeline
  and force a keyframe without dropping the session.
- Viewer side: `jitterBufferTarget = 0` / `playoutDelayHint = 0` to keep browser buffering to a minimum.
- **Risk:** congestion control (GCC) in webrtc-rs is less mature than libwebrtc's.
  **Mitigation:** TWCC interceptor, plus our own estimator fed by RTCP and viewer `getStats()` sent back over `ctl`.
  If M7 benchmarks miss the targets, consider libwebrtc bindings.

## 10. Consent and host UX

- Tray icon states: offline, online, being controlled.
- **Attended mode (default):** a prompt on the host, "*<user> from <device/IP> wants to control this
  computer*", with Allow, Deny, and a 30 s timeout.
- **Unattended mode:** off by default, turned on locally on the host. Optional 6–10 digit PIN, stored on the
  host only as an Argon2id hash. 5 wrong attempts lock it for 15 minutes.
- An always-on-top **"Being controlled by <user> — Disconnect"** banner shows for the whole session. It can't
  be turned off.
- macOS: guided first-run flow for the Screen Recording and Accessibility permissions, with a state check before each session.
- Out of scope for v1 (flagged): controlling the Windows UAC/secure desktop and the login screen. That needs a
  SYSTEM service and per-session helper processes. Planned after M8.

## 11. Security checklist

- TLS everywhere. HSTS. Secrets only from env or config (`CARSON_*`). A `.env.example` with no real values.
- Token-bucket rate limits per IP and per user: login, pairing (code guessing), `session.request`, PIN attempts (on the agent too).
- Audit log (append-only table): `who, device, action, viewer_ip, user_agent, started/ended, bytes, relay?`.
  Visible on the account page.
- Account page lists devices and lets the user revoke them. It also shows active sessions with a force-disconnect.
- SQL only through parameterized queries. Message size limits. Strict CSP on the viewer. CSRF protection on forms.

## 12. Reliability

- WebSocket reconnect with jittered exponential backoff (cap 10 s). The agent keeps its device token across restarts.
- `pc.oniceconnectionstatechange` goes to `disconnected`, then ICE restart after 2 s, then full renegotiation via `session.resume`.
  The session can be resumed for 30 s and is ended after that.
- Network-change detection (online/offline events, interface changes on the agent) triggers an ICE restart right away.
- Server shuts down gracefully: it drains WebSockets and tells clients to reconnect.

## 13. Observability

- Structured JSON logs (Go `log/slog`, Rust `tracing`) with `sid` and `device_id` correlation. No PII in debug logs.
- Prometheus `/metrics`: online devices, active sessions, relay ratio, signaling latency, auth failures.
- Viewer debug overlay (toggle with `Ctrl+Alt+Shift+D`): RTT, fps, bitrate, packet loss, decode time, codec, and candidate type (direct or relay).

## 14. Repo layout

```
carson/
  server/        Go module  github.com/<org>/carson/server   → binary carson-server
  agent/         Rust workspace: carson-agent (bin), carson-capture, carson-encode, carson-input, carson-proto
  viewer/        TS + Vite → carson-viewer static bundle (embedded into carson-server)
  proto/         protocol spec + JSON schema + binary input test vectors (shared by Go/Rust/TS tests)
  deploy/        docker-compose.yml, coturn.conf, Caddyfile
  docs/          design, runbooks, e2e test plan
  .github/workflows/ci.yml   lint + test for all three
```

**Planned dependencies (each justified when it's added):**
Go: `coder/websocket`, `modernc.org/sqlite` (pure Go, so no cgo), `coreos/go-oidc` + `x/oauth2`, `prometheus/client_golang`.
Rust: `webrtc` (webrtc-rs), `tokio`, `tracing`, `keyring`, `screencapturekit`, `windows`, `enigo` or native input, `tray-icon`, `ed25519-dalek`.
Viewer: `vite`, `typescript`, `vitest`. Nothing else at runtime.

## 15. Milestones (each one is a commit series with tests)

| # | Deliverable | Tests |
|---|---|---|
| 1 | Monorepo scaffold, CI (GitHub Actions: Go/Rust/TS lint+test), compose stack, this doc | CI green on empty builds |
| 2 | OIDC login, pairing flow, device registry, heartbeat presence, "My devices" page | authz table tests, pairing expiry/guessing, presence timeouts, WS integration tests |
| 3 | View-only share on LAN: macOS capture → H.264 → WebRTC → browser | signaling state-machine tests, agent pipeline unit tests, manual LAN check |
| 4 | Mouse, keyboard, scroll, text clipboard, monitor switching; Windows capture/input | binary codec vectors shared across languages, key-mapping tables, stuck-key release |
| 5 | coturn and TURN credentials, relay fallback, ICE restart, 30 s resume, WS backoff | reconnect tests with simulated drops (netem/toxiproxy), TURN-only forced test |
| 6 | Consent prompt, banner, unattended + PIN, audit log, revoke, rate limits, DTLS fingerprint signing | security tests: cross-account access, revoked device, PIN lockout, rate limits |
| 7 | HW encoders on all platforms, adaptive ladder, latency benchmark (glass-to-glass timestamp overlay) | benchmark report vs. targets: <100 ms, 30–60 fps |
| 8 | Installers (pkg/dmg, MSI, deb), autostart, docs, E2E test plan | install/uninstall smoke tests, E2E script |

**Local toolchain to install for M1:** Go (current stable) and Rust (rustup). Docker and Node 24 are already present.

## 16. Legal and privacy notes

- **Consent and notice:** the visible banner and attended-mode prompt cover most cases. With **unattended access on
  devices used by other people (e.g. employees)**, workplace-monitoring and wiretap laws (GDPR, ePrivacy, US
  state laws) may require written notice. The product should show a one-time disclosure when turning it on.
- **No recording:** carson does not record screens. If recording is ever added, it needs separate consent and a separate notice.
- **Data minimization:** the audit log stores IPs, which count as personal data under GDPR. Default retention is 90 days,
  set in config. Deleting an account removes devices, sessions, and logs.
- **Codec patents:** H.264 is used only through OS hardware encoders or Cisco's OpenH264 binary, both
  royalty-covered. Don't statically bundle x264 or ffmpeg's H.264 encoder.
- **OAuth app review:** Google sign-in with only basic scopes is free, but may need consent-screen verification for public use.

## 17. Implementation notes (as built)

**M2:**
- GitHub has no user OIDC, so it uses OAuth2 + `/user/emails` (primary **verified** email only). Google uses full OIDC (ID token verified, nonce checked). Both use PKCE and single-use, DB-stored `state` (10 min).
- Identities are linked by provider-verified email, so Google + GitHub sign-ins with the same address are one account. Logins without a verified email are rejected.
- Pairing codes: 8 letters from a 20-consonant alphabet (≈2.6·10¹⁰), stored hashed, 10-minute TTL. Lookups and approvals are rate limited per user and per IP (10/min). The approve screen shows the device name and OS before anything is bound.
- Device auth: `POST /v1/device/challenge` → Ed25519 signature over `carson-device-auth:v1\n<device_id>\n<nonce>` → 15-minute bearer token (hashed at rest). Nonces are single-use, device-bound, and live 60 s. Cross-language test vector: `proto/device-auth.json`.
- Key storage: macOS Keychain / Windows Credential Manager via `keyring-core`. **Linux uses a 0600 file** for now (Secret Service needs a desktop session), to be revisited in M8. `CARSON_AGENT_KEYSTORE=file` forces the file store (CI, e2e).
- Presence: the server pings agents every 15 s, and a missed pong within 30 s marks them offline. The agent also pings and reconnects with 0.5→10 s jittered backoff. A second agent with the same identity replaces the first (close 4000), and the replaced agent exits instead of fighting.
- Viewers get a `hello.ok` snapshot on every (re)connect, then `presence` deltas, so nothing is lost across reconnects.

**M3:**
- Session flow: viewer `session.request` (id-correlated) → server checks ownership in the DB and that the device is online/free → `turn.creds` to both (STUN only for now; TURN in M5) → `session.request` to the agent with viewer email/IP/UA → agent opens the capture source (fails fast with `permission_denied`) → `session.accept` → agent `sdp.offer` (dropped by the server if sent before accept) → trickle ICE → media. Only the two parties of a session can relay its messages, and either side disconnecting sends `session.end` to the other.
- Agent stack: `screencapturekit` 11 (BGRA, newest frame wins) → OpenH264 software encoder (Constrained Baseline, screen-content mode, frame skipping for rate control, IDR on PLI/FIR, safety IDR every 4 s, re-send of the last frame after 1 s idle) → webrtc-rs 0.21 `TrackLocalStaticSample`. One session at a time per device.
- Keyframe requests: webrtc-rs only delivers inbound RTCP to the app when an interceptor marks it, so `keyframe.rs` adds one, adapted from webrtc-rs's example.
- Measured on one MacBook (agent and Chrome viewer on the same machine, host candidates): 1280×720 test pattern at 30 fps, about 0.6 Mbps, 6 ms jitter buffer, 4 ms decode. Real 1440×900 screen at 29 fps. Glass-to-glass latency is not measured yet (M7).
- Build notes: agent dev builds optimize dependencies and the media crates (unoptimized OpenH264 managed only ~9 fps). `MACOSX_DEPLOYMENT_TARGET=14.0` is pinned, and `carson-capture/build.rs` finds the Swift back-compat libraries under both Command Line Tools and full Xcode.
- **Open issues:** no consent prompt or "being viewed" banner yet; the agent only logs the viewer (M6). Browser mDNS (`.local`) candidates are not resolved, but LAN works via peer-reflexive candidates. The OpenH264 source build must become Cisco's binary via `libloading` before distribution (M8).

## 18. Future work (not in M1–M8)

LAN-only serverless mode (mDNS), file transfer, audio, multi-viewer, a desktop viewer app, mobile viewer,
login screen / UAC control, horizontal scaling (Redis presence, sticky WebSockets).
