# Building MacCleaner

## Requirements

- macOS 14 or later on Apple silicon.
- [Rust](https://rustup.rs) (stable, edition 2024 — Rust 1.85+).
- Swift 5.10+ from the **Command Line Tools** (`xcode-select --install`). Xcode is not required.

> With Command Line Tools only, SwiftUI's `@State` macro and String Catalogs are unavailable, so
> the GUI avoids `@State` (it keeps view state in `@Observable` stores) and uses classic
> `.lproj/Localizable.strings` files.

## Build the app

```bash
git clone https://github.com/gadgrandez/maccleaner.git
cd maccleaner
./scripts/build-app.sh
open build/MacCleaner.app
```

`build-app.sh`:

1. builds the Rust static library (`crates/ffi`) and the monitor (`crates/monitor`) in release mode;
2. builds the SwiftUI app (`gui/`, Swift Package) linking that library;
3. assembles `build/MacCleaner.app` (executables, translations from `gui/Localization`, icon,
   `Info.plist`);
4. signs it (see below).

## Package a release

```bash
./scripts/package-release.sh
```

Produces `dist/MacCleaner-<version>-macOS-<arch>.zip` (made with `ditto`, which preserves the
code signature) and its `.sha256`. The version comes from `[workspace.package] version` in
`Cargo.toml`.

## Code signing

macOS ties permissions such as **Full Disk Access** to the app's code signature.

- If a certificate named **"MacCleaner Local Signing"** exists in your keychain, the build is
  signed with it. Its designated requirement does not change between builds, so granted
  permissions survive rebuilds. To create one (self-signed, valid only on your Mac):

  ```bash
  cat > cert.cnf <<'EOF'
  [ req ]
  distinguished_name = dn
  x509_extensions = ext
  prompt = no
  [ dn ]
  CN = MacCleaner Local Signing
  [ ext ]
  basicConstraints = critical,CA:false
  keyUsage = critical,digitalSignature
  extendedKeyUsage = critical,codeSigning
  EOF
  /usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -keyout key.pem -out cert.pem -days 3650 -config cert.cnf
  P=$(/usr/bin/openssl rand -hex 16)
  /usr/bin/openssl pkcs12 -export -inkey key.pem -in cert.pem -name "MacCleaner Local Signing" -out id.p12 -passout pass:$P
  security import id.p12 -k ~/Library/Keychains/login.keychain-db -P "$P" -T /usr/bin/codesign
  rm key.pem id.p12 cert.cnf
  ```

- Otherwise it is signed ad hoc, and permissions must be granted again after each build.
- Use another identity with `SIGN_NAME="Developer ID Application: …" ./scripts/build-app.sh`.
  Public distribution without Gatekeeper warnings requires a Developer ID certificate and
  notarization (`xcrun notarytool`), which are not part of these scripts.

## Tests

```bash
cargo test --workspace
```

Tests use temporary folders; one test that moves a file to the real Trash is ignored by default
(`cargo test -- --ignored` runs it).

## Development tools

| Script | Purpose |
|---|---|
| `scripts/make-icon.swift` | Regenerates `gui/Resources/AppIcon.icns`. |
| `scripts/make-demo-home.sh <dir>` | Creates a fake home folder (~1.7 GB) with junk, projects and files. |
| `scripts/snapshot.sh <dir> section[:scan]…` | Saves PNG screenshots rendered by the app itself. Start the app with `open --env MACCLEANER_SNAPSHOT_DIR=<dir> build/MacCleaner.app`. |
| `crates/core/examples/bench_space.rs`, `bench_dev.rs` | Read-only benchmarks: `cargo run --release -p maccleaner-core --example bench_dev`. |

Environment variables (development only):

- `HOME=<dir>` — the core analyses that folder instead of your home (use it with the demo home).
- `MACCLEANER_SIMULATED_PRESSURE=normal|warning|critical` — simulates memory pressure to test how
  scans throttle and stop.
- `MACCLEANER_SNAPSHOT_DIR=<dir>` — enables `scripts/snapshot.sh`.

## Project layout

```
crates/core      Rust core (all analysis and cleaning logic)
crates/ffi       C API (JSON in/out) used by the GUI — contracts in docs/ffi-contracts.md
crates/monitor   Background monitor (LaunchAgent)
gui/             SwiftUI app; gui/Localization holds the translations
scripts/         Build and development tools
```

## Adding a language

1. Copy `gui/Localization/es.lproj` to `gui/Localization/<lang>.lproj` and translate both
   `Localizable.strings` (UI) and `Core.strings` (texts produced by the Rust core).
2. Add `<string><lang></string>` to `CFBundleLocalizations` in `scripts/build-app.sh`.
