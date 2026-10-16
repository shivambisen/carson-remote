# Run your own carson server for free

The server is small: it handles sign-in, the device list and connection setup, and relays
encrypted traffic only when a direct connection is impossible. A free cloud VM is plenty.

## Option A: Oracle Cloud "Always Free" (recommended, $0)

1. Create an Oracle Cloud account and an **Ampere A1** VM (Always Free: up to 4 cores / 24 GB),
   image **Ubuntu 24.04**. Note its **public IP**.
2. In the VM's **VCN → Security List**, add ingress rules: TCP 80, 443, 3478; UDP 3478 and
   49160-49400.
3. Free domain: at <https://www.duckdns.org> create e.g. `mycarson.duckdns.org` → the public IP.
4. Sign-in: create a free **GitHub OAuth app** (GitHub → Settings → Developer settings → OAuth
   Apps) with callback `https://mycarson.duckdns.org/auth/github/callback`. Google works too
   (callback `/auth/google/callback`).
5. On the VM:

   ```bash
   git clone https://github.com/shivambisen/carson-remote.git && cd carson
   sudo bash deploy/install-server.sh --domain mycarson.duckdns.org \
        --github-id YOUR_CLIENT_ID --github-secret YOUR_CLIENT_SECRET
   ```

   This installs Docker, opens the VM firewall, generates secrets into `/opt/carson/.env`,
   and starts carson-server, coturn (TURN relay) and Caddy (free automatic HTTPS from Let's Encrypt).

6. Open `https://mycarson.duckdns.org`, sign in, then run `carson-agent setup --server
   https://mycarson.duckdns.org` on each computer.

## Option B: any VPS or home server

Same script on any Ubuntu machine with a public IP. At home, forward TCP 80/443/3478 and UDP
3478, 49160-49400 to it.

## Option C: without Docker

Download `carson-server-…-linux-amd64.tar.gz` (or arm64) from the releases, run it behind any
HTTPS reverse proxy with `CARSON_WEB_DIR=./web` and the variables in `env.example`, and run
coturn with `deploy/coturn/turnserver.conf`.

## Keeping it running

- Update: `cd /opt/carson && sudo git pull && sudo docker compose -f deploy/docker-compose.yml --env-file .env --profile prod up -d --build`
- Back up `/opt/carson/.env` and the `carson-data` Docker volume (accounts, devices, audit log).
- Logs: `sudo docker compose -f deploy/docker-compose.yml logs -f carson-server`
- Audit log retention: 90 days by default (`CARSON_AUDIT_RETENTION`).

## Costs and limits

All free: Oracle Always Free VM, DuckDNS name, Let's Encrypt certificate, GitHub/Google
sign-in, GitHub Actions builds for public repos. Relayed sessions use the VM's bandwidth; Oracle's
free tier includes a generous monthly egress allowance. Most sessions are direct and use none.
