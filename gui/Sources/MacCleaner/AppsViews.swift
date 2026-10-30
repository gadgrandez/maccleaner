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
    var confirmingOrphans = false
    // Homebrew: `nil` until loaded or when it isn't installed.
    var brew: BrewReport?
    let brewControls = ListControls<BrewPackage>([.size(\.bytes), .name(\.title)], searchPrompt: loc("Search packages")) {
        [$0.title, $0.name, $0.desc ?? ""]
    }
    var selectedPackage: BrewPackage?
    /// The dependencies nothing needs are shown instead of a package.
    var showingUnused = false
    var brewWorking = false
    var confirmingPackage = false
    var confirmingAutoremove = false

    enum Filter: String, CaseIterable, Identifiable {
        case all, intel, appStore, setapp, homebrew
        var id: String { rawValue }

        var title: String {
            switch self {
            case .all: loc("All apps")
            case .intel: loc("Intel only")
            case .appStore: loc("App Store")
            case .setapp: loc("Setapp")
            case .homebrew: loc("Homebrew")
            }
        }
    }

    /// Homebrew only when it is installed.
    var filters: [Filter] { Filter.allCases.filter { $0 != .homebrew || brew != nil } }

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
        case .homebrew: list = list.filter { cask(of: $0) != nil }
        }
        return controls.apply(list)
    }

    func load(force: Bool = false) async {
        guard force || apps.isEmpty else { return }
        await slot.run { [self] in
            loading = true
            defer { loading = false }
            if let r = await alerts.run({ try await Core.apps() }) { apps = r }
            await loadBrew()
        }
    }

    /// Without Homebrew there is nothing to show: no alert.
    func loadBrew() async {
        brew = try? await Core.brewPackages()
        if let selected = selectedPackage {
            selectedPackage = brew?.packages.first { $0.id == selected.id }
        }
    }

    /// The cask that installed the app: Homebrew uninstalls it.
    func cask(of app: AppInfo) -> BrewPackage? {
        brew?.packages.first { $0.kind == .cask && $0.apps.contains(app.path) }
    }

    var visiblePackages: [BrewPackage] { brewControls.apply(brew?.packages ?? []) }

    var unusedBytes: UInt64 { brew?.unused.reduce(0) { $0 + $1.bytes } ?? 0 }

    func uninstallPackage() async {
        guard let package = selectedPackage else { return }
        brewWorking = true
        defer { brewWorking = false }
        guard await alerts.run({ try await Core.brewUninstall(package) }) != nil else { return }
        selectedPackage = nil
        if package.kind == .cask {
            await load(force: true)
        } else {
            await loadBrew()
        }
    }

    func autoremove() async {
        brewWorking = true
        defer { brewWorking = false }
        guard await alerts.run({ try await Core.brewAutoremove() }) != nil else { return }
        showingUnused = false
        await loadBrew()
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

    var selectAllLeftovers: BulkSelection {
        BulkSelection(leftovers?.items.map(\.path) ?? [], in: Bindable(self).selectedLeftovers)
    }

    func selectAll(in group: OrphanGroup) -> BulkSelection {
        BulkSelection(group.items.map(\.path), in: Bindable(self).selectedOrphans)
    }

    /// Every group on screen.
    var selectAllOrphans: BulkSelection {
        BulkSelection(combining: orphanControls.apply(orphans ?? []).map(selectAll(in:)))
    }

    /// Some selected leftovers are records of casks: Homebrew removes them.
    var removesHomebrewRecords: Bool {
        orphans?.flatMap(\.items).contains { $0.kind == "homebrew_cask" && selectedOrphans.contains($0.path) } ?? false
    }

    var orphanSelectedBytes: UInt64 {
        orphans?.flatMap(\.items).filter { selectedOrphans.contains($0.path) }.reduce(0) { $0 + $1.bytes } ?? 0
    }

    func cleanOrphans() async {
        cleaningOrphans = true
        defer { cleaningOrphans = false }
        let request = UninstallRequest(appPath: nil, leftoverPaths: Array(selectedOrphans), dryRun: false)
        let records = removesHomebrewRecords
        guard await alerts.run({ try await Core.uninstall(request) }) != nil else { return }
        await loadOrphans()
        if records { await loadBrew() }
    }
}

struct UninstallerView: View {
    @Environment(AppState.self) private var state

    var body: some View {
        let store = state.apps
        @Bindable var bindable = store
        ModulePage(.uninstaller, subtitle: loc("Removes apps completely, with their caches, preferences and containers.")) {
            Picker("", selection: $bindable.filter) {
                ForEach(store.filters) { Text($0.title).tag($0) }
            }
            .pickerStyle(.menu).fixedSize()
            if store.filter == .homebrew {
                ListToolbar(controls: store.brewControls)
            } else {
                ListToolbar(controls: store.controls)
            }
        } content: {
            HStack(spacing: 14) {
                Group {
                    if store.filter == .homebrew { packageList(store) } else { appList(store) }
                }
                .frame(minWidth: 300, maxWidth: 380)
                if store.filter == .homebrew {
                    BrewDetail(store: store)
                } else {
                    detail(store)
                }
            }
            .padding(.horizontal, 24)
            .padding(.bottom, 20)
        }
        .task { await store.load() }
        .confirmationDialog(loc("Uninstall \(store.selectedApp?.name ?? "")?"), isPresented: $bindable.confirming) {
            Button(uninstallButtonTitle(store), role: .destructive) { Task { await store.uninstall() } }
        } message: {
            Text(uninstallMessage(store))
        }
    }

    /// A cask's app is deleted by Homebrew, not moved to the Trash.
    private func uninstallButtonTitle(_ store: AppsStore) -> String {
        let size = formatBytes(store.uninstallBytes)
        if store.selectedApp.flatMap(store.cask(of:)) != nil { return loc("Uninstall (\(size))") }
        return state.preferences.deletePermanently ? loc("Delete Permanently (\(size))") : loc("Move to Trash (\(size))")
    }

    private func uninstallMessage(_ store: AppsStore) -> String {
        switch (store.selectedApp.flatMap(store.cask(of:)) != nil, state.preferences.deletePermanently) {
        case (true, false): loc("Homebrew will uninstall the app (it won't go to the Trash). The selected leftovers will go to the Trash.")
        case (true, true): loc("Homebrew will uninstall the app and the selected leftovers will be deleted permanently.")
        case (false, true): loc("The app and the selected leftovers will be deleted permanently.")
        case (false, false): loc("The app and the selected leftovers will go to the Trash.")
        }
    }

    private func packageList(_ store: AppsStore) -> some View {
        ScrollView {
            LazyVStack(spacing: 2) {
                if let unused = store.brew?.unused, !unused.isEmpty, !store.brewControls.isSearching {
                    Button {
                        store.selectedPackage = nil
                        store.showingUnused = true
                    } label: {
                        HStack(spacing: 10) {
                            PackageIcon(symbol: "shippingbox", size: 30)
                            VStack(alignment: .leading, spacing: 1) {
                                Text(loc("Unused dependencies")).lineLimit(1)
                                Text(locCount(unused.count, one: { loc("\($0) package") }, other: { loc("\($0) packages") }))
                                    .font(.caption).foregroundStyle(.secondary)
                            }
                            Spacer(minLength: 4)
                            Text(formatBytes(store.unusedBytes)).font(.callout).monospacedDigit().foregroundStyle(.secondary)
                        }
                        .padding(.horizontal, 10).padding(.vertical, 6)
                        .hoverHighlight(selected: store.showingUnused)
                    }
                    .buttonStyle(.plain)
                    Divider().padding(.vertical, 4)
                }
                ForEach(store.visiblePackages) { package in
                    Button {
                        store.showingUnused = false
                        store.selectedPackage = package
                    } label: {
                        HStack(spacing: 10) {
                            PackageIcon(package: package, size: 30)
                            VStack(alignment: .leading, spacing: 1) {
                                Text(package.title).lineLimit(1)
                                Text(verbatim: package.version).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                            }
                            Spacer(minLength: 4)
                            if package.appsMissing { Pill(text: loc("App deleted"), color: .orange) }
                            if package.kind == .cask { Pill(text: package.kind.title, color: .brown) }
                            Text(formatBytes(package.bytes)).font(.callout).monospacedDigit().foregroundStyle(.secondary)
                        }
                        .padding(.horizontal, 10).padding(.vertical, 6)
                        .hoverHighlight(selected: store.selectedPackage == package)
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(8)
        }
        .surfaceCard()
        .overlay { if store.loading && store.brew == nil { ProgressView() } }
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
                            if store.cask(of: app) != nil { Pill(text: "Homebrew", color: .brown) }
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
                            if store.cask(of: app) != nil { Pill(text: "Homebrew", color: .brown) }
                        }
                        if let id = app.bundleId { Text(id).font(.caption).foregroundStyle(.tertiary) }
                    }
                    Spacer()
                }
                CardSection(title: loc("Leftovers"), trailing: store.leftovers.map { formatBytes($0.totalBytes) },
                            selectAll: store.leftovers.map { _ in store.selectAllLeftovers }) {
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
                    if store.uninstalling {
                        CleanProgressView(title: loc("Uninstalling…"))
                    } else if store.selectedAppIsRunning {
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

// MARK: Homebrew

/// A cask's app icon, or a symbol for formulae and casks without an app.
private struct PackageIcon: View {
    var symbol: String
    var app: String? = nil
    let size: CGFloat

    init(symbol: String, size: CGFloat) {
        self.symbol = symbol
        self.size = size
    }

    init(package: BrewPackage, size: CGFloat) {
        symbol = package.kind == .formula ? "terminal" : "shippingbox"
        app = package.apps.first
        self.size = size
    }

    var body: some View {
        if let app {
            FileIcon(path: app, size: size)
        } else {
            Image(systemName: symbol)
                .font(.system(size: size * 0.45, weight: .semibold))
                .foregroundStyle(.brown)
                .frame(width: size, height: size)
                .background(Color.brown.opacity(0.14), in: RoundedRectangle(cornerRadius: size * 0.22))
                .accessibilityHidden(true)
        }
    }
}

private struct BrewDetail: View {
    @Bindable var store: AppsStore

    var body: some View {
        Group {
            if store.showingUnused, let unused = store.brew?.unused, !unused.isEmpty {
                unusedDetail(unused)
            } else if let package = store.selectedPackage {
                packageDetail(package)
            } else {
                VStack(spacing: 10) {
                    Image(systemName: "hand.point.left.fill").font(.system(size: 36)).foregroundStyle(.secondary)
                    Text(loc("Choose a package to see what it takes up")).foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .surfaceCard()
            }
        }
        .confirmationDialog(loc("Uninstall \(store.selectedPackage?.title ?? "")?"), isPresented: $store.confirmingPackage) {
            Button(loc("Uninstall (\(formatBytes(store.selectedPackage?.bytes ?? 0)))"), role: .destructive) {
                Task { await store.uninstallPackage() }
            }
        } message: {
            if let package = store.selectedPackage { Text(confirmation(package)) }
        }
        .confirmationDialog(loc("Remove \(formatBytes(store.unusedBytes)) of unused dependencies?"), isPresented: $store.confirmingAutoremove) {
            Button(loc("Remove"), role: .destructive) { Task { await store.autoremove() } }
        } message: {
            Text(loc("Homebrew will uninstall them (brew autoremove). They won't go to the Trash."))
        }
    }

    private func confirmation(_ package: BrewPackage) -> String {
        let reinstall = package.kind == .cask ? "brew install --cask \(package.name)" : "brew install \(package.name)"
        var text = loc("Homebrew will uninstall it; it won't go to the Trash. To get it back, run \(reinstall).")
        if package.kind == .formula { text += " " + loc("Projects or scripts that use it may stop working.") }
        if package.service { text += " " + loc("Its background service will be stopped.") }
        return text
    }

    private func header(icon: PackageIcon, title: String, subtitle: String?, @ViewBuilder pills: () -> some View) -> some View {
        HStack(spacing: 14) {
            icon
            VStack(alignment: .leading, spacing: 4) {
                Text(title).font(.title2.weight(.bold))
                HStack(spacing: 6) { pills() }
                if let subtitle { Text(subtitle).font(.caption).foregroundStyle(.tertiary).textSelection(.enabled) }
            }
            Spacer()
        }
    }

    private func packageDetail(_ package: BrewPackage) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            header(icon: PackageIcon(package: package, size: 64), title: package.title,
                   subtitle: package.title == package.name ? nil : package.name) {
                if !package.version.isEmpty { Text(verbatim: "v\(package.version)").foregroundStyle(.secondary) }
                Pill(text: package.kind.title, color: .brown)
                if package.service { Pill(text: loc("Service"), color: .blue) }
            }
            if let desc = package.desc { Text(desc).foregroundStyle(.secondary) }
            if package.appsMissing {
                Label(loc("Its app was deleted without Homebrew, which still lists it as installed. Uninstalling it brings Homebrew up to date."),
                      systemImage: "exclamationmark.triangle.fill")
                    .font(.callout).foregroundStyle(.orange)
            }
            if !package.dependents.isEmpty {
                Label {
                    Text(loc("Required by \(ListFormatter.localizedString(byJoining: package.dependents)). Homebrew won't uninstall it while they are installed."))
                } icon: {
                    Image(systemName: "link")
                }
                .font(.callout).foregroundStyle(.orange)
            }
            if !package.apps.isEmpty {
                CardSection(title: loc("Apps")) {
                    ForEach(package.apps, id: \.self) { app in
                        HStack(spacing: 10) {
                            FileIcon(path: app, size: 22)
                            Text((app as NSString).lastPathComponent)
                            Spacer()
                        }
                    }
                    Text(loc("Its data stays: to remove it too, uninstall the app from All apps.")).font(.caption).foregroundStyle(.secondary)
                }
            }
            Spacer(minLength: 0)
            HStack {
                if store.brewWorking { CleanProgressView(title: loc("Uninstalling…")) }
                Spacer()
                Button { store.confirmingPackage = true } label: {
                    Label(store.brewWorking ? loc("Uninstalling…") : loc("Uninstall · \(formatBytes(package.bytes))"), systemImage: "trash")
                }
                .primaryActionStyle().controlSize(.large)
                .disabled(store.brewWorking || !package.dependents.isEmpty)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
    }

    private func unusedDetail(_ unused: [BrewPackage]) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            header(icon: PackageIcon(symbol: "shippingbox", size: 64), title: loc("Unused dependencies"), subtitle: nil) {
                Pill(text: formatBytes(store.unusedBytes), color: .brown)
            }
            Text(loc("Homebrew installed them for packages that are no longer there. Nothing needs them now."))
                .foregroundStyle(.secondary)
            CardSection(title: loc("Packages"), trailing: unused.count.formatted()) {
                ScrollView {
                    LazyVStack(spacing: 0) {
                        ForEach(unused) { package in
                            HStack(spacing: 10) {
                                PackageIcon(package: package, size: 22)
                                VStack(alignment: .leading, spacing: 1) {
                                    Text(package.title)
                                    if let desc = package.desc { Text(desc).font(.caption).foregroundStyle(.secondary).lineLimit(1) }
                                }
                                Spacer()
                                Text(formatBytes(package.bytes)).monospacedDigit().foregroundStyle(.secondary)
                            }
                            .padding(.vertical, 4)
                        }
                    }
                }
                .frame(maxHeight: .infinity)
            }
            HStack {
                if store.brewWorking { CleanProgressView(title: loc("Removing…")) }
                Spacer()
                Button { store.confirmingAutoremove = true } label: {
                    Label(loc("Remove · \(formatBytes(store.unusedBytes))"), systemImage: "trash")
                }
                .primaryActionStyle().controlSize(.large)
                .disabled(store.brewWorking)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
    }
}

// MARK: Leftovers of deleted apps

struct LeftoversView: View {
    @Environment(AppState.self) private var state

    var body: some View {
        let store = state.apps
        @Bindable var bindable = store
        ModulePage(.leftovers, subtitle: loc("Data from apps that are no longer installed.")) {
            if store.orphans != nil {
                ListToolbar(controls: store.orphanControls, refreshLabel: loc("Search again"), busy: store.loadingOrphans || store.orphanWaiting,
                            disabled: store.cleaningOrphans, selectAll: store.selectAllOrphans) {
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
                                CardSection(title: group.id, trailing: formatBytes(group.totalBytes), selectAll: store.selectAll(in: group)) {
                                    ForEach(group.items) { item in
                                        SelectableRow(isOn: store.selectedOrphans.contains(item.path), title: item.name,
                                                      subtitle: item.detail, bytes: item.bytes, path: item.path) {
                                            FileIcon(path: item.path, size: 22)
                                        } toggle: { store.selectedOrphans.toggle(item.path) }
                                    }
                                }
                            }
                        }
                        .padding(.horizontal, 24).padding(.bottom, 96)
                    }
                    ActionBar {
                        if store.cleaningOrphans {
                            CleanProgressView(title: loc("Removing…"))
                        } else {
                            SelectionSummary(count: store.selectedOrphans.count, bytes: store.orphanSelectedBytes)
                        }
                    } trailing: {
                        Button { store.confirmingOrphans = true } label: {
                            Label(store.cleaningOrphans ? loc("Removing…") : loc("Remove leftovers"), systemImage: "trash")
                        }
                        .primaryActionStyle().controlSize(.large)
                        .disabled(store.selectedOrphans.isEmpty || store.cleaningOrphans)
                    }
                }
            }
        }
        .confirmationDialog(loc("Remove \(formatBytes(store.orphanSelectedBytes)) of leftovers?"), isPresented: $bindable.confirmingOrphans) {
            Button(state.preferences.removeTitle, role: .destructive) { Task { await store.cleanOrphans() } }
        } message: {
            Text(store.removesHomebrewRecords
                 ? state.preferences.recoveryNote + " " + loc("Homebrew removes its records itself (brew uninstall --cask).")
                 : state.preferences.recoveryNote)
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
    var confirming = false
    let controls = ListControls<ExtensionItem>([
        .name(\.name),
        .size(\.bytes),
        .text("kind", loc("Type"), symbol: "puzzlepiece") { $0.kindTitle },
        .text("status", loc("Status"), symbol: "exclamationmark.triangle") { $0.orphan ? "0" : "1" },
    ], searchPrompt: loc("Search extensions")) { [$0.name, $0.kindTitle, $0.origin ?? ""] }

    init(alerts: Alerts) { self.alerts = alerts }

    func load() async {
        loading = true
        defer { loading = false }
        if let r = await alerts.run({ try await Core.extensions() }) { items = r }
        selection.removeAll()
    }

    var visible: [ExtensionItem] { controls.apply(items) }

    /// Kinds on screen, each with its items.
    var groups: [(kind: String, items: [ExtensionItem])] {
        let visible = visible
        return Array(Set(visible.map(\.kind))).sorted().map { kind in (kind, visible.filter { $0.kind == kind }) }
    }

    func selectAll(in items: [ExtensionItem]) -> BulkSelection {
        BulkSelection(items.filter(\.removable).map(\.path), in: Bindable(self).selection)
    }

    /// Every removable item on screen.
    var selectAll: BulkSelection { selectAll(in: visible) }

    var selectedBytes: UInt64 { items.filter { selection.contains($0.path) }.reduce(0) { $0 + $1.bytes } }

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
        @Bindable var bindable = store
        ModulePage(.extensions, subtitle: loc("Plugins and add-ons installed on the system.")) {
            ListToolbar(controls: store.controls, refreshLabel: loc("Refresh"), busy: store.loading, disabled: store.removing,
                        selectAll: store.selectAll) { Task { await store.load() } }
        } content: {
            if store.items.isEmpty && !store.loading {
                EmptyResult(symbol: "puzzlepiece.extension", title: loc("No extensions"), message: loc("No third-party plugins or add-ons are installed."))
            } else {
                ZStack(alignment: .bottom) {
                    ScrollView {
                        LazyVStack(spacing: 12) {
                            ForEach(store.groups, id: \.kind) { kind, group in
                                CardSection(title: group[0].kindTitle, trailing: "\(group.count)",
                                            selectAll: group.contains(where: \.removable) ? store.selectAll(in: group) : nil) {
                                    if group[0].isSystemExtension {
                                        SystemExtensionsNote()
                                    }
                                    ForEach(group) { item in
                                        SelectableRow(isOn: store.selection.contains(item.path), title: item.name,
                                                      subtitle: subtitle(item), bytes: item.bytes, path: item.path,
                                                      disabled: !item.removable,
                                                      badge: item.orphan ? Pill(text: loc("Orphan"), color: .orange) : nil) {
                                            FileIcon(path: item.path, size: 22)
                                        } toggle: { store.selection.toggle(item.path) }
                                        .help(item.orphan ? loc("The app that installed it is no longer installed, but it is still loaded.") : "")
                                    }
                                }
                            }
                        }
                        .padding(.horizontal, 24).padding(.bottom, 96)
                    }
                    ActionBar {
                        if store.removing {
                            CleanProgressView(title: loc("Removing…"))
                        } else {
                            SelectionSummary(count: store.selection.count, bytes: store.selectedBytes)
                        }
                    } trailing: {
                        Button { store.confirming = true } label: { Label(loc("Remove"), systemImage: "trash") }
                            .primaryActionStyle().controlSize(.large).disabled(store.selection.isEmpty || store.removing)
                    }
                }
            }
        }
        .overlay { if store.loading && store.items.isEmpty { ProgressView() } }
        .task { if store.items.isEmpty { await store.load() } }
        .confirmationDialog(loc("Remove \(formatBytes(store.selectedBytes)) of extensions?"), isPresented: $bindable.confirming) {
            Button(state.preferences.removeTitle, role: .destructive) { Task { await store.remove() } }
        } message: {
            Text(state.preferences.recoveryNote)
        }
    }

    private func subtitle(_ item: ExtensionItem) -> String {
        if let origin = item.origin {
            return item.orphan ? loc("Installed by \(origin), which is no longer installed") : loc("Installed by \(origin)")
        }
        return item.scope == "user" ? loc("User") : loc("System · requires administrator")
    }
}

/// System extensions can only be turned off in System Settings.
private struct SystemExtensionsNote: View {
    var body: some View {
        HStack(spacing: Space.s) {
            Image(systemName: "info.circle").foregroundStyle(.secondary)
            Text(loc("macOS only lets you turn these off in System Settings."))
                .font(.callout).foregroundStyle(.secondary)
            Spacer(minLength: Space.s)
            Button(loc("Open Settings")) {
                NSWorkspace.shared.open(URL(string: "x-apple.systempreferences:com.apple.LoginItems-Settings.extension")!)
            }
            .secondaryActionStyle()
        }
        .padding(.horizontal, Space.s)
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
