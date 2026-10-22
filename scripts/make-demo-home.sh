#!/bin/sh
# Builds a fake home folder with realistic junk, projects and files, so screenshots never show
# personal data. Usage: scripts/make-demo-home.sh <dir>   (uses ~1 GB of disk; delete it afterwards)
# Then: HOME=<dir> open -n --env HOME=<dir> build/MacCleaner.app
set -eu

H="${1:?usage: make-demo-home.sh <dir>}"
rm -rf "$H"
mkdir -p "$H"

# file <path> <megabytes>: real (non-sparse) bytes so allocated sizes are realistic.
file() { mkdir -p "$(dirname "$H/$1")"; mkfile "${2}m" "$H/$1"; }
# tree <dir> <files> <kb-each>: many small files, like a dependency folder.
tree() {
    i=0
    while [ $i -lt "$2" ]; do
        mkdir -p "$H/$1/pkg$((i % 25))"
        mkfile "${3}k" "$H/$1/pkg$((i % 25))/f$i.js"
        i=$((i + 1))
    done
}
touch_old() { touch -t "$2" "$H/$1"; }   # touch_old <path> <[[CC]YY]MMDDhhmm>

# Junk: caches, logs, Xcode, simulators.
file "Library/Caches/com.spotify.client/Data/cache.bin" 80
file "Library/Caches/Homebrew/downloads/node--22.tar.gz" 40
file "Library/Caches/com.microsoft.VSCode.ShipIt/update.zip" 30
file "Library/Caches/pip/http/wheels.bin" 25
file "Library/Caches/com.apple.Safari/WebKitCache.db" 15
file "Library/Logs/DiagnosticReports/Example-2025.ips" 3
file "Library/Logs/Adobe/install.log" 2
file "Library/Developer/Xcode/DerivedData/WeatherApp-abcd/Build/Intermediates.bin" 120
file "Library/Developer/Xcode/DerivedData/NotesKit-efgh/Index/index.bin" 60
file "Library/Developer/Xcode/iOS DeviceSupport/iPhone15,2 17.4/Symbols.bin" 90
file "Library/Developer/CoreSimulator/Caches/dyld/cache.bin" 20
mkdir -p "$H/Library/Preferences" "$H/Library/LaunchAgents"
printf 'not a plist' > "$H/Library/Preferences/com.example.broken.plist"

# Leftovers of an uninstalled app.
file "Library/Application Support/com.example.OldEditor/state.db" 12
file "Library/Containers/com.example.OldEditor/Data/cache.bin" 8

# A user launch agent.
cat > "$H/Library/LaunchAgents/com.example.updater.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>com.example.updater</string>
<key>ProgramArguments</key><array><string>/Applications/Example.app/Contents/MacOS/Updater</string></array>
<key>RunAtLoad</key><true/>
</dict></plist>
PLIST

# Browsers (Firefox and Brave profiles).
file "Library/Application Support/Firefox/Profiles/abc.default/places.sqlite" 6
file "Library/Application Support/Firefox/Profiles/abc.default/cookies.sqlite" 2
file "Library/Caches/Firefox/Profiles/abc.default/cache2/entries.bin" 35
file "Library/Application Support/BraveSoftware/Brave-Browser/Default/History" 4
file "Library/Application Support/BraveSoftware/Brave-Browser/Default/Cookies" 1

# Developer projects: one active, others idle for months.
mkdir -p "$H/Projects/web-store" "$H/Projects/old-dashboard" "$H/Projects/rust-cli" "$H/Projects/ios-app" "$H/Projects/ml-notebook"
echo '{"name":"web-store"}' > "$H/Projects/web-store/package.json"
echo '{}' > "$H/Projects/web-store/next.config.js"
tree "Projects/web-store/node_modules" 400 300
file "Projects/web-store/.next/cache/webpack.bin" 30
echo '{"name":"old-dashboard"}' > "$H/Projects/old-dashboard/package.json"
tree "Projects/old-dashboard/node_modules" 300 300
touch_old "Projects/old-dashboard/package.json" 202504011200
printf '[package]\nname = "rust-cli"\n' > "$H/Projects/rust-cli/Cargo.toml"
file "Projects/rust-cli/target/debug/deps.bin" 90
touch_old "Projects/rust-cli/Cargo.toml" 202506011200
echo "platform :ios, '17.0'" > "$H/Projects/ios-app/Podfile"
file "Projects/ios-app/Pods/Firebase/lib.bin" 40
touch_old "Projects/ios-app/Podfile" 202503011200
echo '[project]' > "$H/Projects/ml-notebook/pyproject.toml"
file "Projects/ml-notebook/.venv/lib/python3.12/site-packages/torch.bin" 50
echo 'home = /usr/bin' > "$H/Projects/ml-notebook/.venv/pyvenv.cfg"

# Toolchain caches.
file ".npm/_cacache/content-v2/sha512/blob.bin" 60
file ".bun/install/cache/react.tgz" 30
file ".cargo/registry/cache/index.crates.io/serde.crate" 40
file ".cache/uv/wheels/numpy.whl" 50
file ".gradle/caches/modules-2/files.bin" 30

# Large, old and duplicated files.
file "Movies/Vacation 4K.mov" 300
touch_old "Movies/Vacation 4K.mov" 202407011200
file "Downloads/Design Tool Installer.dmg" 140
touch_old "Downloads/Design Tool Installer.dmg" 202508011200
file "Downloads/Dataset 2024.zip" 110
mkdir -p "$H/Pictures" "$H/Documents" "$H/Desktop"
head -c 6000000 /dev/urandom > "$H/Pictures/IMG_1001.jpg"
cp "$H/Pictures/IMG_1001.jpg" "$H/Desktop/IMG_1001 copy.jpg"
head -c 2500000 /dev/urandom > "$H/Documents/Quarterly Report.pdf"
cp "$H/Documents/Quarterly Report.pdf" "$H/Downloads/Quarterly Report (1).pdf"

# Lets the app's Full Disk Access probe succeed inside the demo home.
mkdir -p "$H/Library/Application Support/com.apple.TCC"
: > "$H/Library/Application Support/com.apple.TCC/TCC.db"

du -sh "$H"
