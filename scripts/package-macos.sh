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

echo "==> Generating icon"
swift scripts/make-icon.swift "$DIST/ScpCommander.iconset"
iconutil -c icns "$DIST/ScpCommander.iconset" -o "$APP/Contents/Resources/ScpCommander.icns"
rm -rf "$DIST/ScpCommander.iconset"

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
    <key>CFBundleIconFile</key><string>ScpCommander</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# Ad-hoc signature so Gatekeeper at least sees a sealed bundle locally.
# For distribution: sign with a Developer ID cert and notarize:
#   codesign --force --deep --options runtime --sign "Developer ID Application: …" "$APP"
#   ditto -c -k --keepParent "$APP" app.zip
#   xcrun notarytool submit app.zip --keychain-profile <profile> --wait
#   xcrun stapler staple "$APP"
codesign --force --deep --sign - "$APP"

echo "==> Zipping"
(cd "$DIST" && ditto -c -k --keepParent ScpCommander.app "ScpCommander-${VERSION}-macos.zip")

echo "Done:"
ls -la "$DIST"
