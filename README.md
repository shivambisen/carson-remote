# glenley

Free remote desktop for your own computers. Sign in, pick one of your devices, and view and
control its screen from a browser. Video, keyboard and mouse go **directly and encrypted**
between your computers; a small self-hosted server introduces them and relays only when a
network blocks direct connections.

- **Free:** no paid services needed (free hosting, free domain, free HTTPS, free codecs).
- **Simple:** install the glenley app, sign in with Google, done: no codes. Connect from
  https://app.glenley.in (or your own server) in any browser. Website: https://glenley.in
- **Private:** only your account's computers, signed end-to-end connections, an activity log,
  and the menu-bar / tray icon always shows when someone is connected (an Allow prompt, an
  on-screen pill and a PIN are optional).

## Quick start

1. **Server** (once, about 10 minutes, $0): follow [docs/self-hosting.md](docs/self-hosting.md)
   (Oracle Cloud Always Free + DuckDNS + one script).
2. **Each computer to reach**: install `glenley-agent` from the
   [releases](../../releases) (or Homebrew), then
   `glenley-agent setup --server https://your-server`.
3. **From anywhere:** open your server in a browser, sign in, click **View**.

Details: [docs/user-guide.md](docs/user-guide.md) · Test plan: [docs/e2e-test-plan.md](docs/e2e-test-plan.md) ·
Benchmarks: [docs/benchmarks.md](docs/benchmarks.md)

| Host OS | Status |
|---|---|
| macOS 14+ (Apple Silicon, Intel) | ✅ hardware encoding, tested |
| Windows 10 1903+ | ⚠️ implemented, compile-checked, **not yet run on Windows** |
| Linux | ❌ host not supported yet (server runs fine on Linux) |

## For developers

Design: [docs/carson-design.md](docs/carson-design.md)

| Component | Path | Stack |
|---|---|---|
| `carson-server` | [server/](server/) | Go: signaling, device registry, presence, auth |
| `glenley-agent` | [agent/](agent/) | Rust: capture, encode, input injection (host) |
| `carson-viewer` | [viewer/](viewer/) | TypeScript + Vite: browser viewer |
| protocol fixtures | [proto/](proto/) | shared by all three test suites |
| deploy | [deploy/](deploy/) | docker compose: server + coturn (+ Caddy in prod) |

## Prerequisites

Go 1.27+, Rust stable (via rustup), Node 22+, Docker.

## Run locally (dev mode)

Dev mode enables an email-only login (no verification) so you can try glenley without OAuth apps.

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
glenley-agent unattended on --pin   # viewers must enter the PIN (5 tries, then 15 min lockout)
glenley-agent unattended off        # back to Allow/Deny prompts
```

The PIN is checked on the host, inside the encrypted connection, and stored only as an
Argon2id hash. The banner still appears in unattended mode.

**Security:** every connection is listed on the **Activity** page (who, when, from which IP and
browser, how it ended). **Remove** on the devices page revokes a device instantly. The viewer
checks that each stream is signed by the device's own key (shown as "key …"; compare it with
`glenley-agent status` on that computer) and refuses a device whose key changed.

**Networks:** connections are direct when possible and fall back to the TURN relay (coturn)
on strict NATs and firewalls. Set `CARSON_TURN_SECRET` (shared with coturn) and `CARSON_TURN_URLS`;
the server hands each session time-limited credentials. Add `?relay=1` to a view URL to force the
relay (diagnostics). Brief network drops are survived: signaling reconnects and resumes the
session for up to 30 s, and the media path restarts ICE automatically.

**macOS permissions:** grant the app that runs the agent (e.g. Terminal) **Screen Recording**
and **Accessibility** (for control) in System Settings → Privacy & Security, then restart it.
Without Accessibility the viewer shows "view only".
`glenley-agent capture-check` verifies capture works. `glenley-agent run --test-pattern` streams a
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

## Releasing

Push a tag `vX.Y.Z`: `.github/workflows/release.yml` builds macOS (arm64, x86_64) and Windows
agents, server tarballs (viewer bundled) for Linux/macOS/Windows, a multi-arch server image on
ghcr.io, and a GitHub Release with checksums. Copy the macOS checksums into
`packaging/homebrew/glenley-agent.rb` in your `homebrew-carson` tap. Release agents are built with
`--no-default-features`, so they never contain source-compiled OpenH264 (Cisco's royalty coverage
applies only to its own binaries, which `setup` downloads from Cisco).

## Status

All eight milestones (M1–M8) are implemented; see the milestone table and as-built notes in [the design doc](docs/carson-design.md). Known gaps are listed there and in the host OS table above.

## License

glenley is free and open source under the [Apache License 2.0](LICENSE). The OpenH264 codec
that `setup` downloads is Cisco's binary under Cisco's own license, not part of this project
("OpenH264 Video Codec provided by Cisco Systems, Inc.").
