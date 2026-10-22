#!/bin/sh
# Builds the Rust core, the background monitor and the SwiftUI app, and packages
# build/MacCleaner.app.
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/build/MacCleaner.app"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"

cd "$ROOT"
cargo build --release -p maccleaner-ffi -p maccleaner-monitor
(cd gui && swift build -c release)

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "gui/.build/release/MacCleaner" "$APP/Contents/MacOS/MacCleaner"
cp "target/release/maccleaner-monitor" "$APP/Contents/MacOS/maccleaner-monitor"
# Translations (<lang>.lproj): macOS picks the device language and also localizes its own texts
# (relative dates, dialog buttons…) for every language listed here.
cp -R gui/Localization/*.lproj "$APP/Contents/Resources/"
if [ -f gui/Resources/AppIcon.icns ]; then
    cp gui/Resources/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"
fi

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key><string>com.maccleaner.app</string>
    <key>CFBundleDevelopmentRegion</key><string>en</string>
    <key>CFBundleLocalizations</key><array><string>en</string><string>es</string></array>
    <key>CFBundleName</key><string>MacCleaner</string>
    <key>CFBundleDisplayName</key><string>MacCleaner</string>
    <key>CFBundleExecutable</key><string>MacCleaner</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>LSMinimumSystemVersion</key><string>14.0</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSPrincipalClass</key><string>NSApplication</string>
</dict>
</plist>
PLIST

# Sign with the local "MacCleaner Local Signing" certificate when available: its designated
# requirement does not change between builds, so macOS keeps permissions such as Full Disk
# Access. Otherwise sign ad hoc (the permission must then be granted again after every build).
# Distributing outside this Mac requires a Developer ID certificate and notarization.
SIGN_NAME="${SIGN_NAME:-MacCleaner Local Signing}"
SIGN_ID="$(security find-certificate -c "$SIGN_NAME" -Z 2>/dev/null | awk '/SHA-1 hash:/ {print $3; exit}')"
if [ -z "$SIGN_ID" ]; then
    echo "Warning: certificate \"$SIGN_NAME\" not found; signing ad hoc." >&2
    SIGN_ID="-"
fi
codesign --force --sign "$SIGN_ID" --identifier com.maccleaner.monitor "$APP/Contents/MacOS/maccleaner-monitor"
codesign --force --sign "$SIGN_ID" "$APP"

du -sh "$APP"
