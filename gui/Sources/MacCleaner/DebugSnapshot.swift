import AppKit

/// Development tool: saves the window as a PNG by drawing it from the app itself, without
/// relying on the macOS capture service (which sometimes fails with "capture failure").
///
/// Only active when the app is launched with `MACCLEANER_SNAPSHOT_DIR`. Each distributed
/// notification `com.maccleaner.debug.snapshot` (object: `<section>` or `<section>:scan`, with the
/// section as `Destination.rawValue`) switches to that section, optionally runs its scan, and
/// writes `<dir>/<section>.png`. See `scripts/snapshot.sh`.
@MainActor
enum DebugSnapshot {
    static let notification = Notification.Name("com.maccleaner.debug.snapshot")
    private static var observer: NSObjectProtocol?

    static func install(_ state: AppState) {
        guard observer == nil,
              let dir = ProcessInfo.processInfo.environment["MACCLEANER_SNAPSHOT_DIR"].map(URL.init(fileURLWithPath:))
        else { return }
        observer = DistributedNotificationCenter.default().addObserver(forName: notification, object: nil, queue: .main) { note in
            let parts = (note.object as? String ?? "").split(separator: ":").map(String.init)
            let section = parts.first.flatMap(Destination.init(rawValue:))
            Task { @MainActor in
                if let section { state.destination = section }
                if let section, parts.dropFirst().first == "scan" { await scan(section, state) }
                // Let SwiftUI finish laying out and animating the change.
                try? await Task.sleep(for: .milliseconds(1200))
                write(to: dir.appendingPathComponent("\(section?.rawValue ?? "window").png"))
            }
        }
    }

    private static func scan(_ section: Destination, _ state: AppState) async {
        switch section {
        case .smartCare: await state.smartCare.scan()
        case .junk: await state.junk.scan()
        case .devJunk: await state.dev.scan()
        case .privacy: await state.privacy.scan()
        case .uninstaller: await state.apps.load(force: true)
        case .leftovers: await state.apps.loadOrphans()
        case .spaceLens: await state.spaceLens.load(userHome, push: false)
        case .largeFiles: await state.largeFiles.scan()
        case .duplicates: await state.duplicates.scan()
        case .status, .maintenance, .launchItems, .processes, .updates, .extensions: break
        }
    }

    private static func write(to url: URL) {
        guard let view = NSApp.windows.first(where: { $0.isVisible && $0.contentView != nil })?.contentView,
              let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds),
              let context = NSGraphicsContext(bitmapImageRep: rep)
        else { return }
        // Off screen the window background isn't drawn: fill it with the current theme's.
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = context
        view.effectiveAppearance.performAsCurrentDrawingAppearance {
            NSColor.windowBackgroundColor.setFill()
            view.bounds.fill()
        }
        NSGraphicsContext.restoreGraphicsState()
        view.displayIgnoringOpacity(view.bounds, in: context)
        try? rep.representation(using: .png, properties: [:])?.write(to: url)
    }
}
