#!/usr/bin/env bash
# Builds carson-server for one OS/arch with the viewer bundled (web/), as a tarball.
#   packaging/server/build.sh linux amd64
set -euo pipefail
GOOS=${1:?goos}; GOARCH=${2:?goarch}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
VERSION=${VERSION:-$(git -C "$ROOT" describe --tags --always 2>/dev/null || echo dev)}
NAME="carson-server-${VERSION#v}-$GOOS-$GOARCH"
STAGE=$(mktemp -d)/$NAME; mkdir -p "$STAGE"
(cd "$ROOT/viewer" && npm ci --silent && npm run build --silent)
cp -R "$ROOT/viewer/dist" "$STAGE/web"
EXT=""; [ "$GOOS" = windows ] && EXT=".exe"
(cd "$ROOT/server" && CGO_ENABLED=0 GOOS=$GOOS GOARCH=$GOARCH go build -trimpath \
  -ldflags "-s -w -X carson/server/internal/httpapi.Version=$VERSION" -o "$STAGE/carson-server$EXT" ./cmd/carson-server)
cp "$ROOT/.env.example" "$STAGE/env.example"
mkdir -p "$ROOT/dist"
tar -C "$(dirname "$STAGE")" -czf "$ROOT/dist/$NAME.tar.gz" "$NAME"
echo "built dist/$NAME.tar.gz (run with CARSON_WEB_DIR=./web)"
