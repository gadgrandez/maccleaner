// swift-tools-version:5.10
import Foundation
import PackageDescription

// The Rust core must be built first with `cargo build --release -p maccleaner-ffi`.
let repoRoot = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
let rustLibDir = repoRoot.appendingPathComponent("target/release").path

let package = Package(
    name: "MacCleaner",
    platforms: [.macOS(.v14)],
    targets: [
        .systemLibrary(name: "CMacCleaner", path: "Sources/CMacCleaner"),
        .executableTarget(
            name: "MacCleaner",
            dependencies: ["CMacCleaner"],
            path: "Sources/MacCleaner",
            linkerSettings: [
                .unsafeFlags(["-L", rustLibDir]),
                .linkedLibrary("iconv"),
                .linkedFramework("Foundation"),
            ]
        ),
    ]
)
