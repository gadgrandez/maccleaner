import SwiftUI

/// Smart Care orchestrates the modules: scans everything at once and cleans only what's safe (the junk checked by default).
@MainActor
@Observable
final class SmartCareStore {
    enum Phase { case idle, scanning, ready, cleaning, done }

    private weak var state: AppState?
    var phase: Phase = .idle
    var cleanedBytes: UInt64 = 0

    func attach(_ state: AppState) { self.state = state }

    func scan() async {
        guard let s = state else { return }
        phase = .scanning
        // In order, not in parallel: several disk walks at once saturate system memory
        // and hurt open apps (e.g. a video call).
        await s.status.refresh()
        if s.status.system?.pressure == .critical {
            s.alerts.message = loc("System memory is under critical pressure. Close apps or tabs you aren't using and scan again.")
            phase = .idle
            return
        }
        let steps: [() async -> Void] = [
            { await s.junk.scan() },
            { await s.dev.scan() },
            { await s.privacy.scan() },
            { await s.apps.loadOrphans() },
            { await s.launch.load() },
            { await s.largeFiles.scan() },
        ]
        for step in steps {
            await step()
            // If a step was cut short (e.g. by memory pressure), partial results aren't shown.
            if s.alerts.message != nil {
                phase = .idle
                return
            }
        }
        phase = .ready
    }

    func run() async {
        guard let s = state else { return }
        phase = .cleaning
        let before = s.junk.selectedBytes
        await s.junk.clean()
        cleanedBytes = before
        phase = .done
    }

    func open(_ destination: Destination) { state?.destination = destination }
}

struct SmartCareView: View {
    @Environment(AppState.self) private var state
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let store = state.smartCare
        ModulePage(.smartCare, subtitle: loc("A one-click scan of your whole Mac: cleanup, privacy, performance, apps and files.")) {
            if store.phase == .ready || store.phase == .done {
                Button { Task { await store.scan() } } label: { Label(loc("Scan again"), systemImage: "arrow.clockwise") }
                    .secondaryActionStyle()
            }
        } content: {
            switch store.phase {
            case .idle, .scanning:
                HeroScan(theme: Destination.smartCare.theme, symbol: "sparkles", title: loc("Tune up your Mac"),
                         message: loc("Scans junk, developer junk, browsing traces, app leftovers, background launch items and large files."),
                         busy: store.phase == .scanning, busyText: loc("Scanning your Mac…")) { Task { await store.scan() } }
            case .ready, .cleaning, .done:
                results(store)
                    .transition(.opacity.combined(with: .scale(scale: 0.98)))
            }
        }
        .animation(reduceMotion ? .easeOut(duration: 0.15) : Motion.standard, value: store.phase)
    }

    private func results(_ store: SmartCareStore) -> some View {
        let junk = state.junk
        let privacyBytes = state.privacy.totalBytes
        let orphanBytes = state.apps.orphans?.reduce(0) { $0 + $1.totalBytes } ?? 0
        let agents = state.launch.items.filter { $0.scope == "user_agent" && $0.enabled }
        let large = state.largeFiles.report?.files ?? []
        return ScrollView {
            VStack(spacing: 16) {
                hero(store, junkBytes: junk.selectedBytes)
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 260), spacing: 14)], spacing: 14) {
                    ResultCard(destination: .junk, value: formatBytes(junk.selectedBytes),
                               detail: loc("of safe junk ready to clean"), action: loc("Review")) { store.open(.junk) }
                    ResultCard(destination: .devJunk, value: formatBytes(state.dev.selectedBytes),
                               detail: loc("in inactive project dependencies and tool caches"), action: loc("Review")) { store.open(.devJunk) }
                    ResultCard(destination: .privacy, value: formatBytes(privacyBytes),
                               detail: loc("in browsing data"), action: loc("Review")) { store.open(.privacy) }
                    ResultCard(destination: .leftovers, value: formatBytes(orphanBytes),
                               detail: loc("of leftovers from deleted apps"), action: loc("Review")) { store.open(.leftovers) }
                    ResultCard(destination: .launchItems, value: "\(agents.count)",
                               detail: loc("user agents active in the background"), action: loc("Manage")) { store.open(.launchItems) }
                    ResultCard(destination: .largeFiles, value: "\(large.count)",
                               detail: loc("files over 100 MB · \(formatBytes(large.reduce(0) { $0 + $1.bytes }))"), action: loc("Review")) { store.open(.largeFiles) }
                    if let s = state.status.system {
                        ResultCard(destination: .status, value: loc("\(formatBytes(s.diskAvailable)) free"),
                                   detail: loc("memory \(formatBytes(s.memUsed)) of \(formatBytes(s.memTotal))"), action: loc("View status")) { store.open(.status) }
                    }
                }
            }
            .padding(.horizontal, 24).padding(.bottom, 24)
        }
    }

    private func hero(_ store: SmartCareStore, junkBytes: UInt64) -> some View {
        HStack(spacing: 20) {
            ZStack {
                Circle().fill(Destination.smartCare.theme.gradient)
                Image(systemName: store.phase == .done ? "checkmark" : "sparkles")
                    .font(.system(size: 34, weight: .bold)).foregroundStyle(.white)
            }
            .frame(width: 84, height: 84)
            .shadow(color: .purple.opacity(0.4), radius: 18, y: 8)
            VStack(alignment: .leading, spacing: 4) {
                switch store.phase {
                case .done:
                    Text(loc("Done: \(formatBytes(store.cleanedBytes)) moved to the Trash")).font(.title2.weight(.bold))
                    Text(loc("Empty the Trash to reclaim the space. The rest of the recommendations are below.")).foregroundStyle(.secondary)
                default:
                    Text(loc("You can safely reclaim \(formatBytes(junkBytes))")).font(.title2.weight(.bold))
                    Text(loc("Only the junk checked by default is cleaned. Everything else needs your review.")).foregroundStyle(.secondary)
                }
            }
            Spacer()
            if store.phase != .done {
                Button { Task { await store.run() } } label: {
                    Label(store.phase == .cleaning ? loc("Cleaning…") : loc("Run"), systemImage: "play.fill").frame(minWidth: 120)
                }
                .primaryActionStyle().controlSize(.extraLarge)
                .disabled(junkBytes == 0 || store.phase == .cleaning)
            }
        }
        .padding(22)
        .surfaceCard(cornerRadius: 24)
    }
}

private struct ResultCard: View {
    let destination: Destination
    let value: String
    let detail: String
    let action: String
    let open: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                ModuleIcon(symbol: destination.symbol, theme: destination.theme, size: 34)
                Text(destination.title).font(.headline)
                Spacer()
            }
            Text(value).font(.title.weight(.semibold)).monospacedDigit()
            Text(detail).font(.callout).foregroundStyle(.secondary).lineLimit(2)
            Button(action, action: open).secondaryActionStyle()
        }
        .padding(18)
        .frame(maxWidth: .infinity, alignment: .leading)
        .surfaceCard()
    }
}
