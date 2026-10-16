#!/usr/bin/env bash
# Builds glenley for macOS, free of charge (ad-hoc code signature, no Apple Developer ID):
#   dist/glenley-<version>-macos-<arch>.zip          glenley.app (menu-bar app; also holds the CLI)
#   dist/glenley-agent-<version>-macos-<arch>.tar.gz the command-line tool alone (Homebrew)
# Downloads are quarantined, so users right-click → Open once; Homebrew installs are not.
#
#   packaging/macos/build.sh [aarch64-apple-darwin|x86_64-apple-darwin]
# Set CARSON_DEFAULT_SERVER=https://… to make "Sign in" go to your server out of the box.
set -euo pipefail
TARGET=${1:-$(rustc -vV | sed -n 's/^host: //p')}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/agent/Cargo.toml" | head -1)
ARCH=${TARGET%%-*}; [ "$ARCH" = aarch64 ] && ARCH=arm64

cd "$ROOT/agent"
rustup target add "$TARGET" >/dev/null 2>&1 || true
# --no-default-features: never ship source-compiled OpenH264 (Cisco covers only its binaries).
cargo build --release -p carson-agent --bins --no-default-features --target "$TARGET"
REL="target/$TARGET/release"

OUT="$ROOT/dist"; mkdir -p "$OUT"
STAGE=$(mktemp -d)

# Signing identity. With MACOS_SIGN_P12_BASE64/_PASSWORD (a fixed self-signed certificate,
# GitHub secrets), every release has the same designated requirement, so macOS keeps Screen
# Recording / Accessibility permissions across updates. Without them: ad-hoc signing (each build
# looks like a new app to macOS). Neither is notarized: Gatekeeper still asks once.
SIGN=-
SIGN_ARGS=()
if [ -n "${MACOS_SIGN_P12_BASE64:-}" ]; then
  KC="$STAGE/sign.keychain-db"
  KCPW=$(openssl rand -hex 16)
  echo "$MACOS_SIGN_P12_BASE64" | base64 --decode > "$STAGE/sign.p12"
  security create-keychain -p "$KCPW" "$KC"
  security set-keychain-settings "$KC"
  security unlock-keychain -p "$KCPW" "$KC"
  security import "$STAGE/sign.p12" -k "$KC" -P "$MACOS_SIGN_P12_PASSWORD" -T /usr/bin/codesign >/dev/null
  security set-key-partition-list -S apple-tool:,apple: -s -k "$KCPW" "$KC" >/dev/null
  # codesign only uses trusted identities; trust this one for code signing (CI runner only).
  openssl pkcs12 -in "$STAGE/sign.p12" -nokeys -passin "pass:$MACOS_SIGN_P12_PASSWORD" -legacy 2>/dev/null > "$STAGE/sign.pem" ||
    openssl pkcs12 -in "$STAGE/sign.p12" -nokeys -passin "pass:$MACOS_SIGN_P12_PASSWORD" > "$STAGE/sign.pem"
  sudo security add-trusted-cert -d -r trustRoot -p codeSign -k /Library/Keychains/System.keychain "$STAGE/sign.pem"
  rm -f "$STAGE/sign.p12"
  SIGN="glenley self-signed"
  SIGN_ARGS=(--keychain "$KC")
  echo "signing with the fixed glenley certificate"
fi

# glenley.app: menu-bar only (LSUIElement), screen-capture purpose string for the permission prompt.
APP="$STAGE/glenley.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$ROOT/packaging/icons/glenley.icns" "$APP/Contents/Resources/glenley.icns"
cp "$REL/glenley" "$REL/glenley-agent" "$APP/Contents/MacOS/"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>in.glenley.agent</string>
  <key>CFBundleName</key><string>glenley</string>
  <key>CFBundleDisplayName</key><string>glenley</string>
  <key>CFBundleExecutable</key><string>glenley</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleIconFile</key><string>glenley</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSScreenCaptureUsageDescription</key>
  <string>glenley shares this screen with your own devices when you connect to this computer.</string>
</dict>
</plist>
PLIST
cp "$ROOT/LICENSE" "$APP/Contents/"
codesign --force --deep --sign "$SIGN" ${SIGN_ARGS[@]+"${SIGN_ARGS[@]}"} --identifier in.glenley.agent "$APP"
codesign --verify --deep "$APP"
codesign -d -r- "$APP" 2>&1 | grep designated
APPZIP="glenley-$VERSION-macos-$ARCH.zip"
(cd "$STAGE" && ditto -c -k --keepParent glenley.app "$OUT/$APPZIP")

# The CLI alone, for Homebrew and scripts.
NAME="glenley-agent-$VERSION-macos-$ARCH"
mkdir "$STAGE/$NAME"
cp "$REL/glenley-agent" "$STAGE/$NAME/"
codesign --force --sign "$SIGN" ${SIGN_ARGS[@]+"${SIGN_ARGS[@]}"} --identifier in.glenley.agent "$STAGE/$NAME/glenley-agent"
cp "$ROOT/packaging/README-agent.txt" "$STAGE/$NAME/README.txt"
cp "$ROOT/LICENSE" "$STAGE/$NAME/"
tar -C "$STAGE" -czf "$OUT/$NAME.tar.gz" "$NAME"
rm -rf "$STAGE"
(cd "$OUT" && shasum -a 256 "$APPZIP" > "$APPZIP.sha256" && shasum -a 256 "$NAME.tar.gz" > "$NAME.tar.gz.sha256")
echo "built $OUT/$APPZIP and $OUT/$NAME.tar.gz"
