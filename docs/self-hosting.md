# Run your own glenley server for free

The server is small: it handles sign-in, the device list and connection setup, and relays
encrypted traffic only when a direct connection is impossible. A free cloud VM is plenty.

## Option A: Oracle Cloud "Always Free" (recommended, $0)

1. **VM.** Create an Oracle Cloud account and an **Ampere A1** VM (Always Free: up to 4 cores /
   24 GB), image **Ubuntu 24.04**. Note its **public IP**. Pick the region closest to you and
   your database.
2. **Firewall.** In the VM's **VCN → Security List**, add ingress rules: TCP 80, 443, 3478; UDP
   3478 and 49160-49400.
3. **Free domain.** At <https://www.duckdns.org> create e.g. `mycarson.duckdns.org` → the public IP.
4. **Google sign-in.** In <https://console.cloud.google.com>: create a project → **APIs & Services
   → OAuth consent screen** (External; app name "glenley"; your email) → **Credentials → Create
   credentials → OAuth client ID** → *Web application*, authorized redirect URI
   `https://mycarson.duckdns.org/auth/google/callback`. Copy the client ID and secret.
   While the consent screen is in *Testing*, add the Google accounts that may sign in under
   **Test users**; **Publish app** to let anyone sign in (basic email/profile scopes need no review).
   GitHub works too: an OAuth app with callback `/auth/github/callback`.
5. **Database (optional).** By default accounts and devices live in a SQLite file on the VM. To
   keep them in managed PostgreSQL instead (free tier at <https://neon.tech>), have its
   connection string ready (`postgresql://…?sslmode=require`).
6. **Install.** On the VM:

   ```bash
   git clone https://github.com/shivambisen/carson-remote.git && cd carson-remote
   sudo bash deploy/install-server.sh --domain mycarson.duckdns.org \
        --google-id YOUR_CLIENT_ID --google-secret YOUR_CLIENT_SECRET \
        --database-url ask        # optional: prompts for the Postgres URL without echoing it
   ```

   This installs Docker, opens the VM firewall, generates secrets into `/opt/carson/.env`,
   and starts carson-server, coturn (TURN relay) and Caddy (free automatic HTTPS from Let's Encrypt).

7. Open `https://mycarson.duckdns.org` and sign in. On each computer to reach, install the glenley
   app and sign in with the same account.

**Make "Sign in" in the app go to your server:** in your GitHub fork, set the repository
variable `CARSON_DEFAULT_SERVER` (Settings → Secrets and variables → Actions → Variables) to
`https://mycarson.duckdns.org` before tagging a release. Without it, people start the app once
with `glenley --server https://mycarson.duckdns.org` (or use `glenley-agent setup`).

## Option R: Render free tier (no card) + a free PostgreSQL (Neon)

[render.yaml](../render.yaml) describes the service. In Render: **New → Blueprint** → pick this
repository → enter `CARSON_DATABASE_URL`, `CARSON_GOOGLE_CLIENT_ID`, `CARSON_GOOGLE_CLIENT_SECRET`
→ **Apply**. Then **Settings → Custom Domains** → add your domain and create the CNAME record it
shows. Edit `CARSON_PUBLIC_URL` in render.yaml (or in the dashboard) to your domain, and use
`https://<domain>/auth/google/callback` as the Google redirect URI.

Limits: free services sleep after 15 minutes without traffic (connected computers keep
reconnecting, which wakes it within about a minute), and there is no TURN relay.

## Option B: any VPS or home server

Same script on any Ubuntu machine with a public IP. At home, forward TCP 80/443/3478 and UDP
3478, 49160-49400 to it.

## Option C: without Docker

Download `carson-server-…-linux-amd64.tar.gz` (or arm64) from the releases, run it behind any
HTTPS reverse proxy with `CARSON_WEB_DIR=./web` and the variables in `env.example`, and run
coturn with `deploy/coturn/turnserver.conf`.

## Keeping it running

- Update: `cd /opt/carson && sudo git pull && sudo docker compose -f deploy/docker-compose.yml --env-file .env --profile prod up -d --build`
- Back up `/opt/carson/.env` and, with SQLite, the `carson-data` Docker volume (accounts, devices, audit log). Neon keeps its own backups.
- Logs: `sudo docker compose -f deploy/docker-compose.yml logs -f carson-server`
- Audit log retention: 90 days by default (`CARSON_AUDIT_RETENTION`).

## Costs and limits

All free: Oracle Always Free VM, DuckDNS name, Let's Encrypt certificate, GitHub/Google
sign-in, GitHub Actions builds for public repos. Relayed sessions use the VM's bandwidth; Oracle's
free tier includes a generous monthly egress allowance. Most sessions are direct and use none.
