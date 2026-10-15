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

## Run locally

```bash
cp .env.example .env
make up            # carson-server on :8080, coturn on :3478
cd viewer && npm install && npm run dev   # viewer on :5173, proxies to :8080
```

Without Docker: `cd server && CARSON_DEV=true go run ./cmd/carson-server`.

## Test and lint

```bash
make test
make lint
```

## Status

Milestone 1 (scaffold, CI, compose, design) is done. See the milestone table in the design doc.
