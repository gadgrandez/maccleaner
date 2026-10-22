#!/bin/sh
# Builds MacCleaner.app and packages it as dist/MacCleaner-<version>-macOS-<arch>.zip for a release.
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
ARCH="$(uname -m)"
ZIP="$ROOT/dist/MacCleaner-$VERSION-macOS-$ARCH.zip"

"$ROOT/scripts/build-app.sh"
mkdir -p "$ROOT/dist"
rm -f "$ZIP"
# ditto keeps the code signature and extended attributes intact (zip -r would not).
ditto -c -k --keepParent "$ROOT/build/MacCleaner.app" "$ZIP"
# Checksum with just the file name, so it can be verified from any folder.
(cd "$ROOT/dist" && shasum -a 256 "$(basename "$ZIP")") | tee "$ZIP.sha256"
