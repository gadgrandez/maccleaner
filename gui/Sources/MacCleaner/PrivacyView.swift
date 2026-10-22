import SwiftUI

@MainActor
@Observable
final class PrivacyStore {
    private let alerts: Alerts
    var report: PrivacyReport?
    var scanning = false
    var cleaning = false
    var confirming = false
    var selected: Set<PrivacyTarget> = []
    var lastClean: CleanReport?
    let controls = ListControls<PrivacySource>([
        .size { $0.items.reduce(0) { $0 + $1.bytes } },
        .name { locCore($0.name) },
    ], searchPrompt: loc("Search browsers")) { [locCore($0.name)] + $0.items.map { locCore($0.name) } }

    private let slot: ScanSlot

    init(alerts: Alerts, scans: ScanCoordinator) {
        self.alerts = alerts
        slot = ScanSlot(loc("Privacy"), coordinator: scans)
    }

    /// Queued, waiting for another scan to finish.
    var isWaiting: Bool { slot.isWaiting }

    /// Sources with something to show; items without files are hidden unless inaccessible.
    var sources: [PrivacySource] {
        (report?.sources ?? []).map { s in
            PrivacySource(id: s.id, name: s.name, running: s.running,
                          items: s.items.filter { !$0.paths.isEmpty || !$0.accessible })
        }.filter { !$0.items.isEmpty }
    }

    var totalBytes: UInt64 { sources.flatMap(\.items).reduce(0) { $0 + $1.bytes } }

    var selectedBytes: UInt64 {
        sources.flatMap { s in s.items.filter { selected.contains(PrivacyTarget(source: s.id, item: $0.id)) } }
            .reduce(0) { $0 + $1.bytes }
    }

    func scan() async {
        await slot.run { [self] in
            scanning = true
            defer { scanning = false }
            guard let r = await alerts.run({ try await Core.privacyScan() }) else { return }
            report = r
            // By default only caches and recent items: history and cookies are chosen by hand.
            selected = Set(r.sources.flatMap { s in
                s.items.filter { ($0.id == "cache" || $0.id == "recent_items") && $0.accessible && !s.running }
                    .map { PrivacyTarget(source: s.id, item: $0.id) }
            })
        }
    }

    func toggle(_ source: PrivacySource, _ item: PrivacyItem) {
        selected.toggle(PrivacyTarget(source: source.id, item: item.id))
    }

    func clean() async {
        let targets = Array(selected)
        guard !targets.isEmpty else { return }
        cleaning = true
        defer { cleaning = false }
        guard let r = await alerts.run({ try await Core.privacyClean(targets) }) else { return }
        lastClean = r
        await scan()
    }
}

struct PrivacyView: View {
    @Environment(AppState.self) private var state

    var body: some View {
        let store = state.privacy
        @Bindable var bindable = store
        ModulePage(.privacy, subtitle: loc("History, cookies and browsing traces from your browsers.")) {
            if store.report != nil {
                ListToolbar(controls: store.controls, busy: store.scanning || store.cleaning || store.isWaiting) { Task { await store.scan() } }
            }
        } content: {
            if store.report == nil {
                HeroScan(theme: Destination.privacy.theme, symbol: "hand.raised.fill", title: loc("Review your traces"),
                         message: loc("Safari, Chrome, Firefox, Edge, Brave, Arc and more. Nothing is deleted without your confirmation."),
                         busy: store.scanning, waiting: store.isWaiting) { Task { await store.scan() } }
            } else if store.sources.isEmpty {
                EmptyResult(symbol: "checkmark.shield.fill", title: loc("No traces"), message: loc("There's no browsing data to clean."))
            } else {
                ZStack(alignment: .bottom) {
                    ScrollView {
                        LazyVGrid(columns: [GridItem(.adaptive(minimum: 340), spacing: 14)], spacing: 14) {
                            ForEach(store.controls.apply(store.sources)) { source in
                                sourceCard(source, store: store)
                            }
                        }
                        .padding(.horizontal, 24)
                        .padding(.bottom, 96)
                    }
                    ActionBar {
                        Text(loc("\(formatBytes(store.selectedBytes)) selected")).font(.headline).monospacedDigit()
                    } trailing: {
                        Button { store.confirming = true } label: {
                            Label(store.cleaning ? loc("Cleaning…") : loc("Remove traces"), systemImage: "hand.raised.slash.fill")
                        }
                        .primaryActionStyle().controlSize(.large)
                        .disabled(store.selected.isEmpty || store.cleaning)
                    }
                }
            }
        }
        .confirmationDialog(loc("Remove the selected traces?"), isPresented: $bindable.confirming) {
            Button(loc("Move to Trash"), role: .destructive) { Task { await store.clean() } }
        } message: {
            Text(loc("You'll be signed out of websites whose cookies you remove. Browsers must be closed."))
        }
    }

    private func sourceCard(_ source: PrivacySource, store: PrivacyStore) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 10) {
                ModuleIcon(symbol: source.id == "system" ? "clock.arrow.circlepath" : "globe", theme: Destination.privacy.theme, size: 32)
                Text(locCore(source.name)).font(.headline)
                Spacer()
                if source.running { Pill(text: loc("open: quit it to clean"), color: .orange) }
            }
            ForEach(source.items) { item in
                SelectableRow(isOn: store.selected.contains(PrivacyTarget(source: source.id, item: item.id)),
                              title: locCore(item.name),
                              subtitle: item.accessible ? nil : loc("Requires Full Disk Access"),
                              bytes: item.bytes,
                              disabled: !item.accessible || source.running) {
                    Image(systemName: icon(for: item.id)).frame(width: 22).foregroundStyle(Destination.privacy.theme.tint)
                } toggle: {
                    store.toggle(source, item)
                }
            }
        }
        .padding(16)
        .surfaceCard()
    }

    private func icon(for item: String) -> String {
        switch item {
        case "history": "clock"
        case "cookies": "birthday.cake"
        case "cache": "internaldrive"
        case "downloads": "arrow.down.circle"
        case "autofill": "character.textbox"
        case "local_storage": "cylinder.split.1x2"
        case "sessions": "rectangle.stack"
        default: "doc"
        }
    }
}
