#!/bin/bash
# Build a .deb of the GTK app into dist/. Run on Ubuntu 24.04+ with the
# prerequisites from the README (plus dpkg-deb, which ships with dpkg).
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"
DIST="$ROOT/dist"
VERSION="${VERSION:-0.1.0}"
ARCH="$(dpkg --print-architecture)"
PKG="$DIST/scp-commander_${VERSION}_${ARCH}"

echo "==> Building (release)"
cargo build -p scp-ubuntu --release --features scp-core/s3

echo "==> Assembling ${PKG}.deb"
rm -rf "$PKG"
mkdir -p "$PKG/DEBIAN" "$PKG/usr/bin" "$PKG/usr/share/applications"

cp target/release/scp-ubuntu "$PKG/usr/bin/scp-commander"
cp packaging/scp-commander.desktop "$PKG/usr/share/applications/"

cat > "$PKG/DEBIAN/control" <<CONTROL
Package: scp-commander
Version: ${VERSION}
Section: net
Priority: optional
Architecture: ${ARCH}
Depends: libgtk-4-1 (>= 4.10), libssh2-1, libssl3t64 | libssl3
Maintainer: SCP Commander
Description: WinSCP-style dual-pane SFTP/FTP/S3 file manager (GTK4)
 Dual-pane file manager with SFTP, FTP/FTPS and S3 support, transfer
 queue with progress, directory sync, and saved sites.
CONTROL

dpkg-deb --build --root-owner-group "$PKG"

echo "Done:"
ls -la "$DIST"
