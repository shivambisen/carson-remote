# carson

Remote desktop for your own devices: sign in on two laptops, pick one, and view and control its
screen over WebRTC. Video and input go peer-to-peer and are end-to-end encrypted. A small server
handles discovery, signaling, and TURN relay fallback.

Design: [docs/carson-design.md](docs/carson-design.md)

| Component | Path | Stack |
|---|---|---|
| `carson-server` | [server/](server/) | Go: signaling, device registry, presence, auth |
| `carson-agent` | [agent/](agent/) | Rust: capture, encode, input injection (host) |
| `carson-viewer` | [viewer/](viewer/) | TypeScript + Vite: browser viewer |
| protocol fixtures | [proto/](proto/) | shared by all three test suites |
| deploy | [deploy/](deploy/) | docker compose: server + coturn (+ Caddy in prod) |

## Prerequisites

Go 1.27+, Rust stable (via rustup), Node 22+, Docker.

## Run locally (dev mode)

Dev mode enables an email-only login (no verification) so you can try carson without OAuth apps.

```bash
# 1. Server + viewer on http://localhost:8080
cd viewer && npm install && npm run build && cd ..
cd server && CARSON_DEV=true CARSON_WEB_DIR=../viewer/dist go run ./cmd/carson-server

# 2. On the computer to share: pair it, then keep it online
cd agent && cargo run -p carson-agent -- pair --server http://localhost:8080
cargo run -p carson-agent -- run
```

Open http://localhost:8080, sign in with any email, go to **Pair a device**, and enter the code
the agent printed. The device appears under **My devices** and turns green while `run` is active.
Click **View** to watch and control its screen. Click the picture to focus it; mouse, keyboard
and scroll then go to the remote computer (uncheck **Control** for view only). Copy/paste text
works both ways: ⌘/Ctrl+V in the viewer pastes your clipboard on the remote side, and text copied
there lands in your clipboard. With several monitors, a display picker appears in the toolbar.
**Fullscreen** also captures system shortcuts in Chrome (hold Esc to leave). **Stats** (or
Ctrl+Alt+Shift+D) shows fps, bitrate, RTT, jitter buffer, decode time, and direct vs relayed path.

Keys are sent as physical keys, so ⌘ on a Mac viewer is the Windows key on a Windows host (and vice
versa). Use the host's own shortcuts (e.g. Ctrl+C on Windows).

**Consent:** by default every connection shows an **Allow / Deny** prompt on the host (no
answer in 30 s = deny). While a session is active, a red always-on-top banner names who is
connected; its **Disconnect** button ends the session. For unattended access (no one at the
computer), run this on the host itself; it can never be switched on remotely:

```bash
carson-agent unattended on --pin   # viewers must enter the PIN (5 tries, then 15 min lockout)
carson-agent unattended off        # back to Allow/Deny prompts
```

The PIN is checked on the host, inside the encrypted connection, and stored only as an
Argon2id hash. The banner still appears in unattended mode.

**Networks:** connections are direct when possible and fall back to the TURN relay (coturn)
on strict NATs and firewalls. Set `CARSON_TURN_SECRET` (shared with coturn) and `CARSON_TURN_URLS`;
the server hands each session time-limited credentials. Add `?relay=1` to a view URL to force the
relay (diagnostics). Brief network drops are survived: signaling reconnects and resumes the
session for up to 30 s, and the media path restarts ICE automatically.

**macOS permissions:** grant the app that runs the agent (e.g. Terminal) **Screen Recording**
and **Accessibility** (for control) in System Settings → Privacy & Security, then restart it.
Without Accessibility the viewer shows "view only".
`carson-agent capture-check` verifies capture works. `carson-agent run --test-pattern` streams a
synthetic pattern instead (no permission needed; input is only logged). Other flags: `--fps`,
`--max-width`, `--bitrate-kbps`, `--display`, `--input native|log|off`, `--no-clipboard`.

For live-reloading viewer work, run the server with `CARSON_PUBLIC_URL=http://localhost:5173`
and `cd viewer && npm run dev`; Vite proxies `/v1`, `/auth` and the WebSocket to `:8080`.

With Docker: `cp .env.example .env && make up` (server + coturn).

### Real sign-in

Create an OAuth app and set `CARSON_GOOGLE_CLIENT_ID/SECRET` and/or `CARSON_GITHUB_CLIENT_ID/SECRET`.
Callback URL: `<CARSON_PUBLIC_URL>/auth/<google|github>/callback`.

## Test and lint

```bash
make test
make lint
scripts/e2e-pairing.sh   # real server + real agent: pair, online, offline
```

## Status

Milestones 1–4 are done: sign-in, pairing, presence, screen share, and remote control (mouse, keyboard, scroll, text clipboard, monitor switching) on macOS hosts. Windows capture/input is implemented but so far only compile-checked. M5 (TURN relay, ICE restart, 30 s resume) is done, along with the consent part of M6. Next: the rest of M6 (audit log, revoke, rate limits). See the milestone table in the design doc.
