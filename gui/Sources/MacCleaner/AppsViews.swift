import SwiftUI

// MARK: Uninstaller

@MainActor
@Observable
final class AppsStore {
    private let alerts: Alerts
    var apps: [AppInfo] = []
    var loading = false
    var filter: Filter = .all
    let controls = ListControls<AppInfo>([
        .name(\.name),
        .size(\.bytes),
        .text("source", loc("Source"), symbol: "bag") { $0.source.rawValue },
    ], searchPrompt: loc("Search apps")) { [$0.name, $0.bundleId ?? ""] }
    let orphanControls = ListControls<OrphanGroup>([.size(\.totalBytes), .name(\.id)], searchPrompt: loc("Search leftovers")) {
        [$0.id] + $0.items.map(\.name)
    }
    var selectedApp: AppInfo?
    var leftovers: AppLeftovers?
    var loadingLeftovers = false
    var selectedLeftovers = Set<String>()
    var uninstalling = false
    var confirming = false
    // Orphaned leftovers
    var orphans: [OrphanGroup]?
    var loadingOrphans = false
    var selectedOrphans = Set<String>()
    var cleaningOrphans = false

    enum Filter: String, CaseIterable, Identifiable {
        case all, intel, appStore, setapp
        var id: String { rawValue }

        var title: String {
            switch self {
            case .all: loc("All apps")
            case .intel: loc("Intel only")
            case .appStore: loc("App Store")
            case .setapp: loc("Setapp")
            }
        }
    }

    private let slot: ScanSlot
    private let orphanSlot: ScanSlot

    init(alerts: Alerts, scans: ScanCoordinator) {
        self.alerts = alerts
        slot = ScanSlot(loc("Applications"), coordinator: scans)
        orphanSlot = ScanSlot(loc("App Leftovers"), coordinator: scans)
    }

    /// Queued, waiting for another scan to finish.
    var isWaiting: Bool { slot.isWaiting }
    var orphanWaiting: Bool { orphanSlot.isWaiting }

    var visibleApps: [AppInfo] {
        var list = apps.filter { !$0.isApple }
        switch filter {
        case .all: break
        case .intel: list = list.filter { $0.archs == ["x86_64"] }
        case .appStore: list = list.filter { $0.source == .appStore }
        case .setapp: list = list.filter { $0.source == .setapp }
        }
        return controls.apply(list)
    }

    func load(force: Bool = false) async {
        guard force || apps.isEmpty else { return }
        await slot.run { [self] in
            loading = true
            defer { loading = false }
            if let r = await alerts.run({ try await Core.apps() }) { apps = r }
        }
    }

    func select(_ app: AppInfo) async {
        selectedApp = app
        leftovers = nil
        loadingLeftovers = true
        defer { loadingLeftovers = false }
        guard let r = await alerts.run({ try await Core.leftovers(appPath: app.path) }), selectedApp == app else { return }
        leftovers = r
        selectedLeftovers = Set(r.items.map(\.path))
    }

    var uninstallBytes: UInt64 {
        (selectedApp?.bytes ?? 0) + (leftovers?.items.filter { selectedLeftovers.contains($0.path) }.reduce(0) { $0 + $1.bytes } ?? 0)
    }

    var selectedAppIsRunning: Bool {
        guard let id = selectedApp?.bundleId else { return false }
        return !NSRunningApplication.runningApplications(withBundleIdentifier: id).isEmpty
    }

    func uninstall() async {
        guard let app = selectedApp else { return }
        uninstalling = true
        defer { uninstalling = false }
        let request = UninstallRequest(appPath: app.path, leftoverPaths: Array(selectedLeftovers), dryRun: false)
        guard let r = await alerts.run({ try await Core.uninstall(request) }) else { return }
        if r.failed > 0 {
            alerts.message = r.outcomes.first { !$0.ok }?.error.map(locCore) ?? loc("Some items couldn't be removed")
        }
        selectedApp = nil
        leftovers = nil
        await load(force: true)
    }

    func loadOrphans() async {
        await orphanSlot.run { [self] in
            loadingOrphans = true
            defer { loadingOrphans = false }
            guard let r = await alerts.run({ try await Core.orphans() }) else { return }
            orphans = r
            selectedOrphans = Set(r.flatMap { $0.items.map(\.path) })
        }
    }

    var orphanSelectedBytes: UInt64 {
        orphans?.flatMap(\.items).filter { selectedOrphans.contains($0.path) }.reduce(0) { $0 + $1.bytes } ?? 0
    }

    func cleanOrphans() async {
        cleaningOrphans = true
        defer { cleaningOrphans = false }
        let request = UninstallRequest(appPath: nil, leftoverPaths: Array(selectedOrphans), dryRun: false)
        guard await alerts.run({ try await Core.uninstall(request) }) != nil else { return }
        await loadOrphans()
    }
}

struct UninstallerView: View {
    @Environment(AppState.self) private var state

    var body: some View {
        let store = state.apps
        @Bindable var bindable = store
        ModulePage(.uninstaller, subtitle: loc("Removes apps completely, with their caches, preferences and containers.")) {
            Picker("", selection: $bindable.filter) {
                ForEach(AppsStore.Filter.allCases) { Text($0.title).tag($0) }
            }
            .pickerStyle(.menu).fixedSize()
            ListToolbar(controls: store.controls)
        } content: {
            HStack(spacing: 14) {
                appList(store)
                    .frame(minWidth: 300, maxWidth: 380)
                detail(store)
            }
            .padding(.horizontal, 24)
            .padding(.bottom, 20)
        }
        .task { await store.load() }
        .confirmationDialog(loc("Uninstall \(store.selectedApp?.name ?? "")?"), isPresented: $bindable.confirming) {
            Button(loc("Move to Trash (\(formatBytes(store.uninstallBytes)))"), role: .destructive) { Task { await store.uninstall() } }
        } message: {
            Text(loc("The app and the selected leftovers will go to the Trash."))
        }
    }

    private func appList(_ store: AppsStore) -> some View {
        ScrollView {
            LazyVStack(spacing: 2) {
                ForEach(store.visibleApps) { app in
                    Button { Task { await store.select(app) } } label: {
                        HStack(spacing: 10) {
                            FileIcon(path: app.path, size: 30)
                            VStack(alignment: .leading, spacing: 1) {
                                Text(app.name).lineLimit(1)
                                Text(app.version.map { "v\($0)" } ?? " ").font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.tail)
                            }
                            Spacer(minLength: 4)
                            if app.archs == ["x86_64"] { Pill(text: "Intel", color: .orange) }
                            Text(formatBytes(app.bytes)).font(.callout).monospacedDigit().foregroundStyle(.secondary)
                        }
                        .padding(.horizontal, 10).padding(.vertical, 6)
                        .hoverHighlight(selected: store.selectedApp == app)
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(8)
        }
        .surfaceCard()
        .overlay { if store.loading && store.apps.isEmpty { ProgressView() } }
    }

    @ViewBuilder
    private func detail(_ store: AppsStore) -> some View {
        if let app = store.selectedApp {
            VStack(alignment: .leading, spacing: 14) {
                HStack(spacing: 14) {
                    FileIcon(path: app.path, size: 64)
                    VStack(alignment: .leading, spacing: 4) {
                        Text(app.name).font(.title2.weight(.bold))
                        HStack(spacing: 6) {
                            if let v = app.version { Text(verbatim: "v\(v)").foregroundStyle(.secondary) }
                            Pill(text: app.archs.joined(separator: " + ").replacingOccurrences(of: "x86_64", with: "Intel").replacingOccurrences(of: "arm64", with: "Apple silicon"),
                                 color: .blue)
                            if app.source != .other { Pill(text: app.source == .appStore ? "App Store" : "Setapp", color: .purple) }
                        }
                        if let id = app.bundleId { Text(id).font(.caption).foregroundStyle(.tertiary) }
                    }
                    Spacer()
                }
                CardSection(title: loc("Leftovers"), trailing: store.leftovers.map { formatBytes($0.totalBytes) }) {
                    if store.loadingLeftovers {
                        ProgressView().frame(maxWidth: .infinity)
                    } else if let l = store.leftovers, !l.items.isEmpty {
                        ScrollView {
                            LazyVStack(spacing: 0) {
                                ForEach(l.items) { item in
                                    SelectableRow(isOn: store.selectedLeftovers.contains(item.path), title: item.name,
                                                  subtitle: item.kindTitle, bytes: item.bytes, path: item.path) {
                                        FileIcon(path: item.path, size: 22)
                                    } toggle: { store.selectedLeftovers.toggle(item.path) }
                                }
                            }
                        }
                        .frame(maxHeight: .infinity)
                    } else {
                        Text(loc("No leftovers found.")).foregroundStyle(.secondary)
                    }
                }
                HStack {
                    if store.selectedAppIsRunning {
                        Label(loc("The app is open: quit it before uninstalling."), systemImage: "exclamationmark.triangle.fill")
                            .foregroundStyle(.orange).font(.callout)
                    }
                    Spacer()
                    Button { store.confirming = true } label: {
                        Label(store.uninstalling ? loc("Uninstalling…") : loc("Uninstall · \(formatBytes(store.uninstallBytes))"), systemImage: "trash")
                    }
                    .primaryActionStyle().controlSize(.large)
                    .disabled(store.uninstalling || store.loadingLeftovers || store.selectedAppIsRunning)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        } else {
            VStack(spacing: 10) {
                Image(systemName: "hand.point.left.fill").font(.system(size: 36)).foregroundStyle(.secondary)
                Text(loc("Choose an app to see everything it takes up")).foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .surfaceCard()
        }
    }
}

// MARK: Leftovers of deleted apps

struct LeftoversView: View {
    @Environment(AppState.self) private var state

    var body: some View {
        let store = state.apps
        ModulePage(.leftovers, subtitle: loc("Data from apps that are no longer installed.")) {
            if store.orphans != nil {
                ListToolbar(controls: store.orphanControls, refreshLabel: loc("Search again"), busy: store.loadingOrphans || store.orphanWaiting) {
                    Task { await store.loadOrphans() }
                }
            }
        } content: {
            if store.orphans == nil {
                HeroScan(theme: Destination.leftovers.theme, symbol: "shippingbox.fill", title: loc("Find leftovers of deleted apps"),
                         message: loc("Containers, preferences and caches left behind when apps were deleted by dragging them to the Trash."),
                         buttonTitle: loc("Search"), busy: store.loadingOrphans, waiting: store.orphanWaiting) { Task { await store.loadOrphans() } }
            } else if let orphans = store.orphans, orphans.isEmpty {
                EmptyResult(symbol: "checkmark.seal.fill", title: loc("No leftovers"), message: loc("There's no data from uninstalled apps."))
            } else if let orphans = store.orphans {
                ZStack(alignment: .bottom) {
                    ScrollView {
                        LazyVStack(spacing: 12) {
                            ForEach(store.orphanControls.apply(orphans)) { group in
                                CardSection(title: group.id, trailing: formatBytes(group.totalBytes)) {
                                    ForEach(group.items) { item in
                                        SelectableRow(isOn: store.selectedOrphans.contains(item.path), title: item.name,
                                                      subtitle: item.kindTitle, bytes: item.bytes, path: item.path) {
                                            FileIcon(path: item.path, size: 22)
                                        } toggle: { store.selectedOrphans.toggle(item.path) }
                                    }
                                }
                            }
                        }
                        .padding(.horizontal, 24).padding(.bottom, 96)
                    }
                    ActionBar {
                        Text(loc("\(formatBytes(store.orphanSelectedBytes)) selected")).font(.headline).monospacedDigit()
                    } trailing: {
                        Button { Task { await store.cleanOrphans() } } label: {
                            Label(store.cleaningOrphans ? loc("Removing…") : loc("Remove leftovers"), systemImage: "trash")
                        }
                        .primaryActionStyle().controlSize(.large)
                        .disabled(store.selectedOrphans.isEmpty || store.cleaningOrphans)
                    }
                }
            }
        }
    }
}

// MARK: Extensions

@MainActor
@Observable
final class ExtensionsStore {
    private let alerts: Alerts
    var items: [ExtensionItem] = []
    var loading = false
    var selection = Set<String>()
    var removing = false
    let controls = ListControls<ExtensionItem>([
        .name(\.name),
        .size(\.bytes),
        .text("kind", loc("Type"), symbol: "puzzlepiece") { $0.kindTitle },
    ], searchPrompt: loc("Search extensions")) { [$0.name, $0.kindTitle] }

    init(alerts: Alerts) { self.alerts = alerts }

    func load() async {
        loading = true
        defer { loading = false }
        if let r = await alerts.run({ try await Core.extensions() }) { items = r }
        selection.removeAll()
    }

    func remove() async {
        removing = true
        defer { removing = false }
        let paths = Array(selection)
        guard await alerts.run({ try await Core.removeExtensions(paths: paths) }) != nil else { return }
        await load()
    }
}

struct ExtensionsView: View {
    @Environment(AppState.self) private var state

    var body: some View {
        let store = state.extensions
        let visible = store.controls.apply(store.items)
        let kinds = Array(Set(visible.map(\.kind))).sorted()
        ModulePage(.extensions, subtitle: loc("Plugins and add-ons installed on the system.")) {
            ListToolbar(controls: store.controls, refreshLabel: loc("Refresh"), busy: store.loading) { Task { await store.load() } }
        } content: {
            if store.items.isEmpty && !store.loading {
                EmptyResult(symbol: "puzzlepiece.extension", title: loc("No extensions"), message: loc("No third-party plugins or add-ons are installed."))
            } else {
                ZStack(alignment: .bottom) {
                    ScrollView {
                        LazyVStack(spacing: 12) {
                            ForEach(kinds, id: \.self) { kind in
                                let group = visible.filter { $0.kind == kind }
                                CardSection(title: group[0].kindTitle, trailing: "\(group.count)") {
                                    ForEach(group) { item in
                                        SelectableRow(isOn: store.selection.contains(item.path), title: item.name,
                                                      subtitle: item.scope == "user" ? loc("User") : loc("System · requires administrator"),
                                                      bytes: item.bytes, path: item.path, disabled: !item.removable) {
                                            FileIcon(path: item.path, size: 22)
                                        } toggle: { store.selection.toggle(item.path) }
                                    }
                                }
                            }
                        }
                        .padding(.horizontal, 24).padding(.bottom, 96)
                    }
                    if !store.selection.isEmpty {
                        ActionBar {
                            Text(locCount(store.selection.count, one: { loc("\($0) extension selected") }, other: { loc("\($0) extensions selected") })).font(.headline)
                        } trailing: {
                            Button { Task { await store.remove() } } label: { Label(loc("Remove"), systemImage: "trash") }
                                .primaryActionStyle().controlSize(.large).disabled(store.removing)
                        }
                    }
                }
            }
        }
        .overlay { if store.loading && store.items.isEmpty { ProgressView() } }
        .task { if store.items.isEmpty { await store.load() } }
    }
}

// MARK: Updates (Sparkle feeds)

struct UpdateInfo: Identifiable, Sendable {
    let app: AppInfo
    let latest: String
    let downloadURL: URL?
    var id: String { app.id }
}

@MainActor
@Observable
final class UpdatesStore {
    private let alerts: Alerts
    var updates: [UpdateInfo]?
    var checking = false
    var checkedCount = 0
    let controls = ListControls<UpdateInfo>([.name(\.app.name), .size(\.app.bytes)], searchPrompt: loc("Search apps")) { [$0.app.name] }

    init(alerts: Alerts) { self.alerts = alerts }

    func check(apps: [AppInfo]) async {
        checking = true
        defer { checking = false }
        let candidates = apps.filter { $0.sparkleFeedUrl != nil && $0.version != nil }
        checkedCount = candidates.count
        var found: [UpdateInfo] = []
        await withTaskGroup(of: UpdateInfo?.self) { group in
            var iterator = candidates.makeIterator()
            func addNext() {
                guard let app = iterator.next() else { return }
                group.addTask { await AppcastChecker.check(app) }
            }
            for _ in 0..<6 { addNext() }
            while let result = await group.next() {
                if let result { found.append(result) }
                addNext()
            }
        }
        updates = found.sorted { $0.app.name.localizedCaseInsensitiveCompare($1.app.name) == .orderedAscending }
    }
}

enum AppcastChecker {
    static func check(_ app: AppInfo) async -> UpdateInfo? {
        guard let feed = app.sparkleFeedUrl.flatMap(URL.init(string:)), let current = app.version else { return nil }
        var request = URLRequest(url: feed, timeoutInterval: 12)
        request.setValue("MacCleaner/0.1", forHTTPHeaderField: "User-Agent")
        guard let (data, _) = try? await URLSession.shared.data(for: request) else { return nil }
        let parser = AppcastParser()
        guard let best = parser.parse(data).max(by: { compareVersions($0.version, $1.version) == .orderedAscending }),
              compareVersions(best.version, current) == .orderedDescending else { return nil }
        return UpdateInfo(app: app, latest: best.version, downloadURL: best.url)
    }

    static func compareVersions(_ a: String, _ b: String) -> ComparisonResult {
        let pa = a.split(whereSeparator: { !$0.isNumber }).compactMap { Int($0) }
        let pb = b.split(whereSeparator: { !$0.isNumber }).compactMap { Int($0) }
        for i in 0..<max(pa.count, pb.count) {
            let x = i < pa.count ? pa[i] : 0, y = i < pb.count ? pb[i] : 0
            if x != y { return x < y ? .orderedAscending : .orderedDescending }
        }
        return .orderedSame
    }
}

/// Minimal Sparkle appcast parser: display version and download URL of each `item`.
final class AppcastParser: NSObject, XMLParserDelegate {
    struct Entry { let version: String; let url: URL? }
    private var entries: [Entry] = []
    private var inItem = false
    private var element = ""
    private var text = ""
    private var shortVersion: String?
    private var version: String?
    private var url: URL?

    func parse(_ data: Data) -> [Entry] {
        let parser = XMLParser(data: data)
        parser.delegate = self
        parser.parse()
        return entries
    }

    func parser(_ parser: XMLParser, didStartElement name: String, namespaceURI: String?, qualifiedName: String?, attributes: [String: String]) {
        element = qualifiedName ?? name
        text = ""
        if name == "item" {
            inItem = true
            shortVersion = nil; version = nil; url = nil
        } else if inItem && name == "enclosure" {
            shortVersion = shortVersion ?? attributes["sparkle:shortVersionString"]
            version = version ?? attributes["sparkle:version"]
            url = attributes["url"].flatMap(URL.init(string:))
        }
    }

    func parser(_ parser: XMLParser, foundCharacters string: String) { text += string }

    func parser(_ parser: XMLParser, didEndElement name: String, namespaceURI: String?, qualifiedName: String?) {
        let value = text.trimmingCharacters(in: .whitespacesAndNewlines)
        switch qualifiedName ?? name {
        case "sparkle:shortVersionString" where inItem && !value.isEmpty: shortVersion = value
        case "sparkle:version" where inItem && !value.isEmpty: version = version ?? value
        case "item":
            inItem = false
            if let v = shortVersion ?? version { entries.append(Entry(version: v, url: url)) }
        default: break
        }
        text = ""
    }
}

struct UpdatesView: View {
    @Environment(AppState.self) private var state

    var body: some View {
        let store = state.updates
        let apps = state.apps
        ModulePage(.updates, subtitle: loc("Checks for new versions of apps that update with Sparkle.")) {
            if store.updates != nil {
                ListToolbar(controls: store.controls, refreshLabel: loc("Check"), busy: store.checking) {
                    Task { await apps.load(); await store.check(apps: apps.apps) }
                }
            }
        } content: {
            if store.updates == nil {
                HeroScan(theme: Destination.updates.theme, symbol: "arrow.down.app.fill", title: loc("Find updates"),
                         message: loc("Queries the update feed of each compatible app. App Store and Setapp apps update from their own store."),
                         buttonTitle: loc("Check"), busy: store.checking) {
                    Task { await apps.load(); await store.check(apps: apps.apps) }
                }
            } else if let updates = store.updates, updates.isEmpty {
                EmptyResult(symbol: "checkmark.seal.fill", title: loc("All up to date"),
                            message: locCount(store.checkedCount, one: { loc("\($0) compatible app was checked.") }, other: { loc("\($0) compatible apps were checked.") }))
            } else if let updates = store.updates {
                ScrollView {
                    LazyVStack(spacing: 10) {
                        ForEach(store.controls.apply(updates)) { u in
                            HStack(spacing: 14) {
                                FileIcon(path: u.app.path, size: 40)
                                VStack(alignment: .leading, spacing: 3) {
                                    Text(u.app.name).font(.headline)
                                    Text(verbatim: "\(u.app.version ?? "?")  →  \(u.latest)").foregroundStyle(.secondary).monospacedDigit()
                                }
                                Spacer()
                                Button(loc("Open app")) { NSWorkspace.shared.open(URL(fileURLWithPath: u.app.path)) }
                                    .secondaryActionStyle()
                                if let url = u.downloadURL {
                                    Button(loc("Download")) { NSWorkspace.shared.open(url) }.primaryActionStyle()
                                }
                            }
                            .padding(14)
                            .surfaceCard()
                        }
                        Text(locCount(store.checkedCount, one: { loc("\($0) compatible app was checked.") }, other: { loc("\($0) compatible apps were checked.") })).font(.caption).foregroundStyle(.secondary)
                    }
                    .padding(.horizontal, 24).padding(.bottom, 24)
                }
            }
        }
    }
}
