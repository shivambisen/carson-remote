#!/usr/bin/env bash
# One-command carson server on a fresh Ubuntu 22.04/24.04 VM (e.g. Oracle Cloud "Always Free").
# Installs Docker, opens the firewall, generates secrets, and starts server + TURN + HTTPS.
#
#   sudo bash install-server.sh --domain carson.example.com \
#        --google-id ID --google-secret SECRET            # and/or --github-id/--github-secret
#        [--database-url ask]                              # PostgreSQL (e.g. Neon) instead of SQLite
#
# --database-url ask prompts for the URL without echoing it (keeps the password out of your
# shell history); a literal URL works too.
#
# Free domain: create one at https://www.duckdns.org pointing at this VM's public IP.
# Sign-in: create a free GitHub OAuth app (Settings > Developer settings > OAuth Apps) with
#   callback URL https://<domain>/auth/github/callback
set -euo pipefail

DOMAIN="" GITHUB_ID="" GITHUB_SECRET="" GOOGLE_ID="" GOOGLE_SECRET="" DATABASE_URL=""
REPO="${CARSON_REPO:-https://github.com/shivambisen/carson-remote.git}" DIR=/opt/carson
while [ $# -gt 0 ]; do
  case "$1" in
    --domain) DOMAIN=$2; shift 2 ;;
    --github-id) GITHUB_ID=$2; shift 2 ;;
    --github-secret) GITHUB_SECRET=$2; shift 2 ;;
    --google-id) GOOGLE_ID=$2; shift 2 ;;
    --google-secret) GOOGLE_SECRET=$2; shift 2 ;;
    --repo) REPO=$2; shift 2 ;;
    --database-url) DATABASE_URL=$2; shift 2 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done
[ "$(id -u)" = 0 ] || { echo "run with sudo" >&2; exit 1; }
[ -n "$DOMAIN" ] || { echo "--domain is required (a name pointing at this server)" >&2; exit 1; }
if [ -z "$GITHUB_ID$GOOGLE_ID" ]; then
  echo "Give --google-id/--google-secret or --github-id/--github-secret so people can sign in." >&2; exit 1
fi
if [ "$DATABASE_URL" = ask ]; then
  read -rsp "PostgreSQL URL (postgresql://…): " DATABASE_URL; echo
fi
case "$DATABASE_URL" in ""|postgres://*|postgresql://*) ;; *) echo "--database-url must start with postgresql://" >&2; exit 1 ;; esac

echo "==> Docker"
if ! command -v docker >/dev/null; then
  apt-get update -qq && apt-get install -y -qq docker.io docker-compose-v2 git curl openssl >/dev/null
  systemctl enable --now docker
fi

echo "==> Firewall (HTTP/HTTPS, TURN 3478 tcp+udp, relay 49160-49400/udp)"
open_port() { # proto port
  if command -v ufw >/dev/null && ufw status | grep -q active; then ufw allow "$2/$1" >/dev/null; fi
  # Oracle's Ubuntu images ship iptables rules that REJECT everything not explicitly allowed.
  iptables -C INPUT -p "$1" --dport "$2" -j ACCEPT 2>/dev/null || iptables -I INPUT 5 -p "$1" --dport "$2" -j ACCEPT
}
open_port tcp 80; open_port tcp 443; open_port tcp 3478; open_port udp 3478; open_port udp 49160:49400
command -v netfilter-persistent >/dev/null && netfilter-persistent save >/dev/null 2>&1 || true
echo "    (cloud consoles have their own firewall too: on Oracle, add the same ingress rules to the VCN security list)"

echo "==> Code"
if [ -d "$DIR/.git" ]; then git -C "$DIR" pull -q; else git clone -q "$REPO" "$DIR"; fi
cd "$DIR"

echo "==> Configuration"
PUBLIC_IP=$(getent ahostsv4 "$DOMAIN" | awk 'NR==1{print $1}')
PRIVATE_IP=$(hostname -I | awk '{print $1}')
[ -n "$PUBLIC_IP" ] || { echo "$DOMAIN does not resolve yet; point it at this server first" >&2; exit 1; }
if [ ! -f .env ]; then
  umask 077
  cat > .env <<ENV
CARSON_DEV=false
CARSON_DOMAIN=$DOMAIN
CARSON_PUBLIC_URL=https://$DOMAIN
CARSON_TRUST_PROXY=true
CARSON_TURN_SECRET=$(openssl rand -hex 32)
CARSON_TURN_URLS=turn:$DOMAIN:3478?transport=udp,turn:$DOMAIN:3478?transport=tcp
CARSON_STUN_URLS=stun:$DOMAIN:3478
CARSON_TURN_EXTERNAL_IP=$PUBLIC_IP/$PRIVATE_IP
CARSON_GITHUB_CLIENT_ID=$GITHUB_ID
CARSON_GITHUB_CLIENT_SECRET=$GITHUB_SECRET
CARSON_GOOGLE_CLIENT_ID=$GOOGLE_ID
CARSON_GOOGLE_CLIENT_SECRET=$GOOGLE_SECRET
CARSON_DATABASE_URL=$DATABASE_URL
ENV
  echo "    wrote $DIR/.env (secrets generated; keep it private)"
else
  echo "    keeping existing $DIR/.env"
fi

echo "==> Start"
docker compose -f deploy/docker-compose.yml --env-file .env --profile prod up -d --build
echo
echo "carson is starting at https://$DOMAIN (the HTTPS certificate is issued automatically)."
echo "On each computer to share: install the carson app and sign in, or run"
echo "    carson-agent setup --server https://$DOMAIN"
