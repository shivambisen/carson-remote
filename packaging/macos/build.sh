#!/usr/bin/env bash
# Builds a release carson-agent for macOS and packs it as a tarball, free of charge:
# ad-hoc code signature (no Apple Developer ID needed). Homebrew installs are not
# quarantined, so Gatekeeper does not block them; for direct downloads users right-click → Open once.
#
#   packaging/macos/build.sh [aarch64-apple-darwin|x86_64-apple-darwin]
set -euo pipefail
TARGET=${1:-$(rustc -vV | sed -n 's/^host: //p')}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/agent/Cargo.toml" | head -1)
ARCH=${TARGET%%-*}; [ "$ARCH" = aarch64 ] && ARCH=arm64

cd "$ROOT/agent"
rustup target add "$TARGET" >/dev/null 2>&1 || true
# --no-default-features: never ship source-compiled OpenH264 (Cisco covers only its binaries).
cargo build --release -p carson-agent --no-default-features --target "$TARGET"
BIN="target/$TARGET/release/carson-agent"
codesign --force --sign - --identifier com.carson.agent "$BIN"
codesign --verify "$BIN"

OUT="$ROOT/dist"; mkdir -p "$OUT"
NAME="carson-agent-$VERSION-macos-$ARCH"
STAGE=$(mktemp -d); mkdir "$STAGE/$NAME"
cp "$BIN" "$STAGE/$NAME/"
cp "$ROOT/packaging/README-agent.txt" "$STAGE/$NAME/README.txt"
tar -C "$STAGE" -czf "$OUT/$NAME.tar.gz" "$NAME"
rm -rf "$STAGE"
(cd "$OUT" && shasum -a 256 "$NAME.tar.gz" > "$NAME.tar.gz.sha256")
echo "built $OUT/$NAME.tar.gz"
