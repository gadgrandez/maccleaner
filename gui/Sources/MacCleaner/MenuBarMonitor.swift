import AppKit
import ServiceManagement
import SwiftUI

/// The menu bar monitor runs as a separate, lightweight process: MacCleaner's own executable
/// inside `Contents/Library/LoginItems/MacCleaner Menu.app`, started at login by a user
/// LaunchAgent (like the monitor). It only polls the system status, so it doesn't load any of
/// the app's modules.
@MainActor
enum MenuBarHelper {
    static let bundleIdentifier = "com.maccleaner.menu"
    static let appBundleIdentifier = "com.maccleaner.app"

    /// Starts (or stops) the helper and its start at login; does nothing outside an app bundle
    /// (`swift run`).
    static func apply(enabled: Bool) {
        guard let program = helperExecutable else { return }
        removeLegacyLoginItem()
        Task {
            if enabled {
                _ = try? await Core.enableMenuBar(program: program)
            } else {
                _ = try? await Core.disableMenuBar()
            }
        }
    }

    /// Earlier builds registered the helper with SMAppService, whose registration stops working
    /// after an update; the LaunchAgent replaces it.
    private static func removeLegacyLoginItem() {
        let legacy = SMAppService.loginItem(identifier: bundleIdentifier)
        if legacy.status != .notRegistered { try? legacy.unregister() }
    }

    private static var helperExecutable: String? {
        let url = Bundle.main.bundleURL.appendingPathComponent("Contents/Library/LoginItems/MacCleaner Menu.app/Contents/MacOS/MacCleaner Menu")
        return FileManager.default.isExecutableFile(atPath: url.path) ? url.path : nil
    }

    /// Opens (or brings forward) MacCleaner from the helper: its bundle is four levels up.
    static func openMainApp() {
        let enclosing = Bundle.main.bundleURL.deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
        let url = enclosing.pathExtension == "app"
            ? enclosing
            : NSWorkspace.shared.urlForApplication(withBundleIdentifier: appBundleIdentifier)
        guard let url else { return }
        NSWorkspace.shared.openApplication(at: url, configuration: NSWorkspace.OpenConfiguration())
    }
}

/// Entry point of the helper process: only the menu bar item.
struct MenuBarApp: App {
    // No @State: in the macOS 27 SDK it is a macro that Command Line Tools doesn't ship.
    private let store = MenuBarStore()

    init() {
        // A single instance, however it was launched (by launchd or by opening the bundle).
        let others = NSRunningApplication.runningApplications(withBundleIdentifier: MenuBarHelper.bundleIdentifier)
            .filter { $0.processIdentifier != ProcessInfo.processInfo.processIdentifier }
        if !others.isEmpty { exit(0) }
        Task { [store] in await store.run() }
    }

    var body: some Scene {
        MenuBarExtra {
            MenuBarPanel(store: store)
        } label: {
            MenuBarLabel(store: store)
        }
        .menuBarExtraStyle(.window)
    }
}

/// Readings of the menu bar monitor, refreshed every few seconds while it is shown.
@MainActor
@Observable
final class MenuBarStore {
    static let interval: Duration = .seconds(3)

    var system: SystemStatus?
    /// CPU usage (0…1) since the previous reading; `nil` until there are two readings.
    var cpu: Double?
    private var lastTicks: [UInt64]?

    func refresh() async {
        guard let s = try? await Core.systemStatus() else { return }
        if let last = lastTicks { cpu = Self.cpuUsage(from: last, to: s.cpuTicks) }
        lastTicks = s.cpuTicks
        system = s
    }

    /// Share of non-idle ticks between two cumulative readings (user, system, idle, nice).
    static func cpuUsage(from old: [UInt64], to new: [UInt64]) -> Double? {
        guard old.count == 4, new.count == 4 else { return nil }
        let delta = zip(old, new).map { $1 >= $0 ? $1 - $0 : 0 }
        let total = delta.reduce(0, +)
        guard total > 0 else { return nil }
        return Double(total - delta[2]) / Double(total)
    }

    /// Polls for as long as the process runs.
    func run() async {
        while !Task.isCancelled {
            await refresh()
            try? await Task.sleep(for: Self.interval)
        }
    }
}

/// Menu bar label: a gauge and the CPU usage.
struct MenuBarLabel: View {
    let store: MenuBarStore

    var body: some View {
        HStack(spacing: 3) {
            Image(systemName: "gauge.with.dots.needle.33percent")
            Text(store.cpu.map { percent($0) } ?? "—").monospacedDigit()
        }
        .accessibilityLabel(loc("CPU \(store.cpu.map { percent($0) } ?? "—")"))
    }
}

/// Panel shown when clicking the menu bar item.
struct MenuBarPanel: View {
    let store: MenuBarStore

    var body: some View {
        VStack(alignment: .leading, spacing: Space.m) {
            HStack {
                Text(verbatim: "MacCleaner").font(.headline)
                Spacer()
                if let s = store.system {
                    Pill(text: loc("Memory pressure: \(s.pressure.title)"), color: s.pressure.colors[0])
                }
            }
            if let s = store.system {
                meters(s)
            } else {
                ProgressView().frame(maxWidth: .infinity, minHeight: 120)
            }
            Divider()
            HStack(spacing: Space.s) {
                Button { MenuBarHelper.openMainApp() } label: {
                    Label(loc("Open MacCleaner"), systemImage: "macwindow")
                }
                .primaryActionStyle()
                Spacer()
                Button { NSApp.terminate(nil) } label: { Image(systemName: "power") }
                    .help(loc("Quit the menu bar monitor until the next login"))
            }
            .buttonStyle(.borderless)
        }
        .padding(Space.l)
        .frame(width: 300)
    }

    private func meters(_ s: SystemStatus) -> some View {
        let diskUsed = s.diskTotal - min(s.diskAvailable, s.diskTotal)
        let diskFrac = Double(diskUsed) / Double(max(s.diskTotal, 1))
        let memFrac = Double(s.memUsed) / Double(max(s.memTotal, 1))
        let swapFrac = s.swapTotal > 0 ? Double(s.swapUsed) / Double(s.swapTotal) : 0
        return VStack(spacing: Space.m) {
            MeterRow(symbol: "cpu", title: loc("CPU"),
                     value: store.cpu.map { percent($0) } ?? "—",
                     detail: loc("load \(String(format: "%.2f", s.load.first ?? 0))"),
                     fraction: store.cpu ?? 0, colors: [.pink, .orange])
            MeterRow(symbol: "memorychip", title: loc("Memory"), value: percent(memFrac),
                     detail: loc("\(formatBytes(s.memUsed)) of \(formatBytes(s.memTotal))"),
                     fraction: memFrac, colors: s.pressure.colors)
            MeterRow(symbol: "internaldrive", title: loc("Disk"), value: percent(diskFrac),
                     detail: loc("\(formatBytes(s.diskAvailable)) free"),
                     fraction: diskFrac, colors: diskFrac > 0.9 ? [.red, .orange] : [.teal, .blue])
            MeterRow(symbol: "arrow.left.arrow.right", title: loc("Swap"), value: formatBytes(s.swapUsed),
                     detail: loc("of \(formatBytes(s.swapTotal))"),
                     fraction: swapFrac, colors: swapFrac > 0.75 ? [.orange, .red] : [.indigo, .purple])
        }
    }
}

/// One resource of the menu bar panel: name, value and a bar.
private struct MeterRow: View {
    let symbol: String
    let title: String
    let value: String
    let detail: String
    let fraction: Double
    let colors: [Color]

    var body: some View {
        VStack(alignment: .leading, spacing: Space.xs) {
            HStack(spacing: Space.s) {
                Image(systemName: symbol).foregroundStyle(colors[0]).frame(width: 18)
                Text(title).font(.callout.weight(.medium))
                Spacer(minLength: Space.s)
                Text(detail).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                Text(value).font(.callout.weight(.semibold)).monospacedDigit()
            }
            ProgressView(value: min(max(fraction, 0), 1))
                .tint(LinearGradient(colors: colors, startPoint: .leading, endPoint: .trailing))
        }
        .accessibilityElement(children: .combine)
    }
}

private func percent(_ fraction: Double) -> String {
    fraction.formatted(.percent.precision(.fractionLength(0)))
}
