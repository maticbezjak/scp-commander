#!/bin/bash
# Build a release ScpCommander.app bundle (and zip) into dist/.
# Run from anywhere; requires the macOS prerequisites from the README.
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"
DIST="$ROOT/dist"
APP="$DIST/ScpCommander.app"
VERSION="${VERSION:-0.1.0}"

echo "==> Building Rust core (release)"
cargo build -p scp-core --release --features s3

echo "==> Building Swift app (release)"
(cd ui-macos && SCP_CORE_LIB=../target/release swift build -c release)

echo "==> Assembling ${APP}"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp ui-macos/.build/release/ScpCommander "$APP/Contents/MacOS/ScpCommander"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key><string>ScpCommander</string>
    <key>CFBundleIdentifier</key><string>net.manto.ScpCommander</string>
    <key>CFBundleName</key><string>SCP Commander</string>
    <key>CFBundleDisplayName</key><string>SCP Commander</string>
    <key>CFBundleVersion</key><string>${VERSION}</string>
    <key>CFBundleShortVersionString</key><string>${VERSION}</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>LSMinimumSystemVersion</key><string>13.0</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# Ad-hoc signature so Gatekeeper at least sees a sealed bundle locally.
codesign --force --deep --sign - "$APP"

echo "==> Zipping"
(cd "$DIST" && ditto -c -k --keepParent ScpCommander.app "ScpCommander-${VERSION}-macos.zip")

echo "Done:"
ls -la "$DIST"
