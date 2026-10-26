import SwiftUI

@MainActor
@Observable
final class JunkStore {
    private let alerts: Alerts
    var report: ScanReport?
    var scanning = false
    var cleaning = false
    var confirming = false
    var selectedCategories: Set<String> = []
    var excluded: Set<String> = []
    var expanded: Set<String> = []
    var lastClean: CleanReport?
    /// Items belonging to open apps: shown, but not checked by default.
    var inUse: Set<String> = []
    let controls = ListControls<ScanItem>([
        .size(\.bytes),
        .name(\.name),
        .number("files", loc("Files"), symbol: "doc.on.doc") { $0.files },
    ], searchPrompt: loc("Search items")) { [$0.name, $0.path] }

    private let slot: ScanSlot

    init(alerts: Alerts, scans: ScanCoordinator) {
        self.alerts = alerts
        slot = ScanSlot(loc("System Junk"), coordinator: scans)
    }

    /// Queued, waiting for another scan to finish.
    var isWaiting: Bool { slot.isWaiting }

    /// Cache folders owned by an open app (by bundle id or vendor folder).
    private static func inUsePaths(_ report: ScanReport) -> Set<String> {
        let running = Set(NSWorkspace.shared.runningApplications.compactMap(\.bundleIdentifier))
        let vendorFolders: [String: String] = [
            "Google": "com.google.Chrome", "Microsoft Edge": "com.microsoft.edgemac",
            "BraveSoftware": "com.brave.Browser", "Firefox": "org.mozilla.firefox",
            "Arc": "company.thebrowser.Browser", "Vivaldi": "com.vivaldi.Vivaldi",
        ]
        var result: Set<String> = []
        for cat in report.categories where cat.id == "user_caches" {
            for item in cat.items {
                let name = item.name
                let owned = running.contains { id in name == id || name.hasPrefix(id + ".") }
                    || vendorFolders[name].map(running.contains) == true
                if owned { result.insert(item.path) }
            }
        }
        return result
    }

    var categories: [CategoryReport] { report?.categories.filter { $0.bytes > 0 || $0.errors > 0 } ?? [] }

    /// Display only: categories with matches, sorted by the same criterion as their items.
    var visibleCategories: [CategoryReport] {
        let shown = controls.isSearching ? categories.filter { $0.items.contains(where: controls.matches) } : categories
        let key: (CategoryReport, CategoryReport) -> Bool = switch controls.sortID {
        case "name": { locCore($0.name).localizedStandardCompare(locCore($1.name)) == .orderedAscending }
        case "files": { $0.files < $1.files }
        default: { $0.bytes < $1.bytes }
        }
        return shown.sorted { controls.ascending ? key($0, $1) : key($1, $0) }
    }

    func scan() async {
        await slot.run { [self] in
            scanning = true
            defer { scanning = false }
            guard let r = await alerts.run({ try await Core.scan() }) else { return }
            // Keep what the user unchecked by hand between scans.
            let manual = excluded.subtracting(inUse)
            report = r
            selectedCategories = Set(r.categories.filter { $0.defaultSelected && $0.bytes > 0 }.map(\.id))
            inUse = Self.inUsePaths(r)
            let paths = Set(r.categories.flatMap { $0.items.map(\.path) })
            excluded = inUse.union(manual.intersection(paths))
        }
    }

    func isSelected(_ item: ScanItem, in cat: CategoryReport) -> Bool {
        selectedCategories.contains(cat.id) && !excluded.contains(item.path)
    }

    func toggle(_ item: ScanItem, in cat: CategoryReport) {
        if !selectedCategories.contains(cat.id) {
            selectedCategories.insert(cat.id)
            excluded.formUnion(cat.items.map(\.path))
            excluded.remove(item.path)
        } else if excluded.contains(item.path) {
            excluded.remove(item.path)
        } else {
            excluded.insert(item.path)
        }
    }

    func toggle(_ cat: CategoryReport) {
        if selectedCategories.contains(cat.id) {
            selectedCategories.remove(cat.id)
        } else {
            selectedCategories.insert(cat.id)
            excluded.subtract(cat.items.map(\.path))
        }
    }

    func selectedBytes(in cat: CategoryReport) -> UInt64 {
        guard selectedCategories.contains(cat.id) else { return 0 }
        return cat.items.filter { !excluded.contains($0.path) }.reduce(0) { $0 + $1.bytes }
    }

    private var selectedItems: [(CategoryReport, ScanItem)] {
        categories.filter { selectedCategories.contains($0.id) }
            .flatMap { cat in cat.items.filter { !excluded.contains($0.path) }.map { (cat, $0) } }
    }

    var selectedBytes: UInt64 { selectedItems.reduce(0) { $0 + $1.1.bytes } }
    var deletesPermanently: Bool { selectedItems.contains { $0.0.action == .delete } }

    func clean() async {
        let paths = selectedItems.map(\.1.path)
        guard !paths.isEmpty else { return }
        cleaning = true
        defer { cleaning = false }
        guard let r = await alerts.run({ try await Core.clean(paths: paths) }) else { return }
        lastClean = r
        await scan()
    }
}

struct JunkView: View {
    @Environment(AppState.self) private var state
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let store = state.junk
        @Bindable var bindable = store
        ModulePage(.junk, subtitle: loc("Caches, logs and development leftovers that are safe to delete.")) {
            if store.report != nil {
                ListToolbar(controls: store.controls, busy: store.scanning || store.cleaning || store.isWaiting) { Task { await store.scan() } }
            }
        } content: {
            if store.report == nil || store.scanning && store.report == nil {
                HeroScan(theme: Destination.junk.theme, symbol: "trash.fill", title: loc("Find junk on your Mac"),
                         message: state.preferences.deletePermanently
                             ? loc("Checks caches, logs, Xcode and simulator data, Mail downloads and more. Everything is deleted permanently.")
                             : loc("Checks caches, logs, Xcode and simulator data, Mail downloads and more. Everything goes to the Trash."),
                         busy: store.scanning, waiting: store.isWaiting) { Task { await store.scan() } }
            } else if store.categories.isEmpty {
                EmptyResult(symbol: "checkmark.seal.fill", title: loc("All clean"), message: loc("No junk was found."))
            } else {
                ZStack(alignment: .bottom) {
                    ScrollView {
                        LazyVStack(spacing: 12) {
                            if let r = store.report {
                                summary(r)
                            }
                            ForEach(Array(store.visibleCategories.enumerated()), id: \.element.id) { i, cat in
                                JunkCategoryCard(store: store, category: cat)
                                    .transition(Motion.appear(i, reduceMotion: reduceMotion))
                            }
                        }
                        .animation(Motion.standard, value: store.report?.elapsedMs)
                        .padding(.horizontal, 24)
                        .padding(.bottom, 96)
                    }
                    ActionBar {
                        if store.cleaning {
                            CleanProgressView()
                        } else {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(loc("\(formatBytes(store.selectedBytes)) selected")).font(.headline).monospacedDigit()
                                if let last = store.lastClean {
                                    LastCleanSummary(report: last, cleanedBytes: last.removedBytes)
                                }
                            }
                        }
                    } trailing: {
                        Button { store.confirming = true } label: {
                            Label(store.cleaning ? loc("Cleaning…") : loc("Clean"), systemImage: "sparkles").frame(minWidth: 110)
                        }
                        .primaryActionStyle()
                        .controlSize(.large)
                        .disabled(store.selectedBytes == 0 || store.cleaning || store.scanning)
                    }
                }
            }
        }
        .confirmationDialog(loc("Clean \(formatBytes(store.selectedBytes))?"), isPresented: $bindable.confirming) {
            let preferences = state.preferences
            Button(store.deletesPermanently && !preferences.deletePermanently ? loc("Clean (includes permanent deletion)") : preferences.removeTitle,
                   role: .destructive) {
                Task { await store.clean() }
            }
        } message: {
            Text(store.deletesPermanently && !state.preferences.deletePermanently
                 ? loc("Items in the Trash are deleted forever; everything else is moved to the Trash.")
                 : state.preferences.recoveryNote)
        }
    }

    private func summary(_ r: ScanReport) -> some View {
        HStack(spacing: 12) {
            StatTile(symbol: "externaldrive.fill", theme: Destination.junk.theme, value: formatBytes(r.totalBytes), label: loc("Found"))
            StatTile(symbol: "checkmark.circle.fill", theme: ModuleTheme(colors: [.blue, .teal]),
                     value: formatBytes(state.junk.selectedBytes), label: loc("Selected"))
            StatTile(symbol: "timer", theme: ModuleTheme(colors: [.gray, .secondary]),
                     value: "\(r.elapsedMs) ms", label: loc("Scan time"))
        }
    }
}

private struct JunkCategoryCard: View {
    let store: JunkStore
    let category: CategoryReport

    var body: some View {
        // While searching, categories expand to show the matches.
        let expanded = store.expanded.contains(category.id) || store.controls.isSearching
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 12) {
                Toggle("", isOn: Binding(get: { store.selectedCategories.contains(category.id) }, set: { _ in store.toggle(category) }))
                    .toggleStyle(.checkbox).labelsHidden()
                VStack(alignment: .leading, spacing: 3) {
                    HStack(spacing: 6) {
                        Text(locCore(category.name)).font(.headline)
                        if category.action == .delete { Pill(text: loc("permanent"), color: .red) }
                        if !category.defaultSelected { Pill(text: loc("review"), color: .orange) }
                    }
                    Text(locCore(category.description)).font(.callout).foregroundStyle(.secondary).lineLimit(2)
                    if category.errors > 0 {
                        Label(loc("\(category.errors.formatted()) not accessible: grant Full Disk Access to include them"), systemImage: "lock.fill")
                            .font(.caption).foregroundStyle(.orange)
                    }
                }
                Spacer(minLength: 12)
                VStack(alignment: .trailing, spacing: 2) {
                    Text(formatBytes(category.bytes)).font(.title3.weight(.semibold)).monospacedDigit()
                    Text(countItems(category.items.count)).font(.caption).foregroundStyle(.secondary)
                }
                Button {
                    withAnimation(Motion.standard) { store.expanded.toggle(category.id) }
                } label: {
                    Image(systemName: "chevron.right").rotationEffect(.degrees(expanded ? 90 : 0))
                }
                .buttonStyle(.borderless)
                .disabled(category.items.isEmpty)
                .accessibilityLabel(expanded ? loc("Hide items in \(locCore(category.name))") : loc("Show items in \(locCore(category.name))"))
            }
            .padding(16)
            if expanded {
                Divider().padding(.horizontal, 16)
                LazyVStack(spacing: 0) {
                    ForEach(store.controls.apply(category.items)) { item in
                        SelectableRow(isOn: store.isSelected(item, in: category), title: item.name,
                                      subtitle: store.inUse.contains(item.path)
                                          ? loc("In use by an open app · \(countFiles(item.files))")
                                          : countFiles(item.files),
                                      bytes: item.bytes, path: item.path) {
                            FileIcon(path: item.path, size: 22)
                        } toggle: {
                            store.toggle(item, in: category)
                        }
                        .padding(.horizontal, 16)
                    }
                }
                .padding(.vertical, 6)
            }
        }
        .surfaceCard()
    }
}
