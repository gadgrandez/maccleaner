import SwiftUI

// MARK: Maintenance

@MainActor
@Observable
final class MaintenanceStore {
    private let alerts: Alerts
    var tasks: [MaintenanceTask] = []
    var selected: Set<String> = []
    var results: [String: TaskResult] = [:]
    var running = false

    init(alerts: Alerts) { self.alerts = alerts }

    func load() async {
        guard tasks.isEmpty, let t = await alerts.run({ try await Core.maintenanceTasks() }) else { return }
        tasks = t
        selected = Set(t.filter { !$0.requiresAdmin }.map(\.id))
    }

    func run() async {
        let ids = tasks.map(\.id).filter(selected.contains)
        guard !ids.isEmpty else { return }
        running = true
        defer { running = false }
        guard let r = await alerts.run({ try await Core.runMaintenance(ids: ids) }) else { return }
        for result in r { results[result.id] = result }
    }
}

struct MaintenanceView: View {
    @Environment(AppState.self) private var state

    var body: some View {
        let store = state.maintenance
        ModulePage(.maintenance, subtitle: loc("One-off tasks that fix slowness and odd behavior.")) {
            ZStack(alignment: .bottom) {
                ScrollView {
                    LazyVGrid(columns: [GridItem(.adaptive(minimum: 300), spacing: 14)], spacing: 14) {
                        ForEach(store.tasks) { task in
                            taskCard(task, store: store)
                        }
                    }
                    .padding(.horizontal, 24)
                    .padding(.bottom, 96)
                }
                ActionBar {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(locCount(store.selected.count, one: { loc("\($0) task selected") }, other: { loc("\($0) tasks selected") })).font(.headline)
                        if store.tasks.contains(where: { $0.requiresAdmin && store.selected.contains($0.id) }) {
                            Text(loc("You'll be asked for the administrator password only once.")).font(.caption).foregroundStyle(.secondary)
                        }
                    }
                } trailing: {
                    Button { Task { await store.run() } } label: {
                        Label(store.running ? loc("Running…") : loc("Run"), systemImage: "play.fill").frame(minWidth: 110)
                    }
                    .primaryActionStyle().controlSize(.large)
                    .disabled(store.selected.isEmpty || store.running)
                }
            }
        }
        .task { await store.load() }
    }

    private func taskCard(_ task: MaintenanceTask, store: MaintenanceStore) -> some View {
        let on = store.selected.contains(task.id)
        return HStack(alignment: .top, spacing: 12) {
            ModuleIcon(symbol: task.icon, theme: Destination.maintenance.theme, size: 36)
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 6) {
                    Text(locCore(task.name)).font(.headline)
                    if task.requiresAdmin { Image(systemName: "lock.fill").font(.caption).foregroundStyle(.secondary).help(loc("Requires administrator")) }
                }
                Text(locCore(task.description)).font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                if let r = store.results[task.id] {
                    Label(r.ok ? loc("Completed") : (r.error.map(locCore) ?? loc("Error")), systemImage: r.ok ? "checkmark.circle.fill" : "exclamationmark.triangle.fill")
                        .font(.caption).foregroundStyle(r.ok ? .green : .orange)
                }
            }
            Spacer(minLength: 0)
            Toggle(locCore(task.name), isOn: Binding(get: { on }, set: { _ in store.selected.toggle(task.id) }))
                .toggleStyle(.checkbox).labelsHidden()
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .surfaceCard()
    }
}

// MARK: Login & background

@MainActor
@Observable
final class LaunchStore {
    private let alerts: Alerts
    var items: [LaunchItem] = []
    var loading = false
    var busy: Set<String> = []
    var showSystem = false
    let controls = ListControls<LaunchItem>([
        .name { $0.appName ?? $0.label },
        .number("state", loc("Status"), symbol: "circle.fill") { ($0.loaded ? 2 : 0) + ($0.enabled ? 1 : 0) },
        .text("program", loc("Program"), symbol: "terminal") { $0.program ?? $0.label },
    ], searchPrompt: loc("Search services")) { [$0.label, $0.program ?? "", $0.appName ?? ""] }

    init(alerts: Alerts) { self.alerts = alerts }

    func load() async {
        loading = true
        defer { loading = false }
        if let r = await alerts.run({ try await Core.launchItems() }) { items = r }
    }

    func setEnabled(_ item: LaunchItem, _ enabled: Bool) async {
        busy.insert(item.id)
        defer { busy.remove(item.id) }
        guard let updated = await alerts.run({ try await Core.setLaunchItem(label: item.label, enabled: enabled) }) else { return }
        if let i = items.firstIndex(where: { $0.id == item.id }) { items[i] = updated }
    }

    func remove(_ item: LaunchItem) async {
        busy.insert(item.id)
        defer { busy.remove(item.id) }
        guard await alerts.run({ try await Core.removeLaunchItem(plistPath: item.plistPath) }) != nil else { return }
        await load()
    }

    /// Stops a job left running by a deleted app.
    func stop(_ item: LaunchItem) async {
        busy.insert(item.id)
        defer { busy.remove(item.id) }
        guard await alerts.run({ try await Core.stopLaunchItem(label: item.label) }) != nil else { return }
        await load()
    }
}

struct LaunchItemsView: View {
    @Environment(AppState.self) private var state

    var body: some View {
        let store = state.launch
        @Bindable var bindable = store
        ModulePage(.launchItems, subtitle: loc("Agents and services that start on their own and use resources in the background.")) {
            Toggle(loc("Show system items"), isOn: $bindable.showSystem).toggleStyle(.switch).controlSize(.small)
            ListToolbar(controls: store.controls, refreshLabel: loc("Refresh"), busy: store.loading) { Task { await store.load() } }
        } content: {
            ScrollView {
                LazyVStack(spacing: 14) {
                    ForEach(["user_agent", "global_agent", "global_daemon"], id: \.self) { scope in
                        let group = store.controls.apply(store.items.filter { $0.scope == scope })
                        if !group.isEmpty && (scope == "user_agent" || store.showSystem) {
                            CardSection(title: group[0].scopeTitle, trailing: "\(group.count)") {
                                ForEach(group) { item in row(item, store: store) }
                            }
                        }
                    }
                    if !store.showSystem {
                        Text(loc("Global agents and daemons require administrator privileges and are shown for information only."))
                            .font(.caption).foregroundStyle(.secondary).frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
                .padding(.horizontal, 24)
                .padding(.bottom, 24)
            }
            .overlay { if store.loading && store.items.isEmpty { ProgressView() } }
        }
        .task { await store.load() }
    }

    private func row(_ item: LaunchItem, store: LaunchStore) -> some View {
        HStack(spacing: 12) {
            Circle().fill(item.loaded ? Color.green : Color.secondary.opacity(0.4)).frame(width: 8, height: 8)
                .help(item.loaded ? loc("Loaded") : loc("Not loaded"))
                .accessibilityLabel(item.loaded ? loc("Loaded") : loc("Not loaded"))
            VStack(alignment: .leading, spacing: 2) {
                Text(item.appName ?? item.label).lineLimit(1)
                Text(item.program ?? item.parentBundleId ?? item.label).font(.caption).foregroundStyle(.secondary)
                    .lineLimit(1).truncationMode(.middle)
            }
            Spacer(minLength: 8)
            if item.orphaned { Pill(text: loc("app deleted"), color: .red) }
            if item.runAtLoad { Pill(text: loc("at login"), color: .blue) }
            if item.keepAlive { Pill(text: loc("always on"), color: .orange) }
            if store.busy.contains(item.id) {
                ProgressView().controlSize(.small)
            } else if item.plistPath.isEmpty {
                // Registered by an app that no longer exists: there is no file, only the running job.
                Button(loc("Stop")) { Task { await store.stop(item) } }
                    .controlSize(.small)
                    .help(loc("It keeps running until you log out because its app was deleted."))
            } else if item.removable {
                Toggle("", isOn: Binding(get: { item.enabled }, set: { v in Task { await store.setEnabled(item, v) } }))
                    .toggleStyle(.switch).labelsHidden().controlSize(.small)
                Menu {
                    Button(loc("Show in Finder")) { NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: item.plistPath)]) }
                    Button(loc("Remove…"), role: .destructive) { Task { await store.remove(item) } }
                } label: { Image(systemName: "ellipsis.circle") }
                    .menuStyle(.borderlessButton).fixedSize()
                    .accessibilityLabel(loc("More actions for \(item.appName ?? item.label)"))
            } else {
                RevealButton(path: item.plistPath)
            }
        }
        .padding(.vertical, 4)
    }
}

// MARK: Processes

@MainActor
@Observable
final class ProcessStore {
    struct Row: Identifiable {
        let entry: ProcessEntry
        let cpu: Double
        var id: Int32 { entry.pid }
    }

    private let alerts: Alerts
    var rows: [Row] = []
    let controls = ListControls<Row>([
        .number("memory", loc("Memory"), symbol: "memorychip") { $0.entry.memory },
        .number("cpu", loc("CPU"), symbol: "cpu") { $0.cpu },
        .name(\.entry.name),
    ], searchPrompt: loc("Search processes")) { [$0.entry.name, $0.entry.path ?? ""] }
    private var lastSample: (time: ContinuousClock.Instant, cpu: [Int32: UInt64])?

    init(alerts: Alerts) { self.alerts = alerts }

    func refresh() async {
        guard let list = try? await Core.topProcesses(limit: 40) else { return }
        let now = ContinuousClock.now
        var cpu: [Int32: UInt64] = [:]
        for p in list { cpu[p.pid] = p.cpuTimeNs }
        let previous = lastSample
        rows = list.map { p in
            var pct = 0.0
            if let previous, let before = previous.cpu[p.pid], p.cpuTimeNs >= before {
                let elapsed = now - previous.time
                let ns = Double(elapsed.components.seconds) * 1e9 + Double(elapsed.components.attoseconds) / 1e9
                if ns > 0 { pct = Double(p.cpuTimeNs - before) / ns * 100 }
            }
            return Row(entry: p, cpu: pct)
        }
        lastSample = (now, cpu)
    }

    func quit(_ entry: ProcessEntry) {
        if let app = NSRunningApplication(processIdentifier: entry.pid) {
            app.terminate()
        } else if entry.userOwned {
            kill(entry.pid, SIGTERM)
        }
    }
}

struct ProcessesView: View {
    @Environment(AppState.self) private var state

    var body: some View {
        let store = state.processes
        let maxMem = store.rows.map(\.entry.memory).max() ?? 1
        ModulePage(.processes, subtitle: loc("What uses the most memory and CPU right now. Updates every 2 s.")) {
            ListToolbar(controls: store.controls)
        } content: {
            ScrollView {
                LazyVStack(spacing: 0) {
                    ForEach(store.controls.apply(store.rows)) { row in
                        HStack(spacing: 12) {
                            if let path = row.entry.path, let app = appBundle(path) {
                                FileIcon(path: app, size: 26)
                            } else {
                                Image(systemName: "gearshape.2").frame(width: 26).foregroundStyle(.secondary)
                            }
                            VStack(alignment: .leading, spacing: 4) {
                                Text(row.entry.name).lineLimit(1)
                                ProgressView(value: Double(row.entry.memory), total: Double(maxMem)).tint(Destination.processes.theme.tint)
                            }
                            Text(formatBytes(row.entry.memory)).monospacedDigit().frame(width: 90, alignment: .trailing)
                            Text(String(format: "%.1f %%", row.cpu)).monospacedDigit().foregroundStyle(.secondary).frame(width: 70, alignment: .trailing)
                            Button { store.quit(row.entry) } label: { Image(systemName: "xmark.circle") }
                                .buttonStyle(.borderless)
                                .disabled(!row.entry.userOwned)
                                .help(loc("Quit"))
                                .accessibilityLabel(loc("Quit \(row.entry.name)"))
                        }
                        .padding(.horizontal, 16).padding(.vertical, 8)
                        Divider().padding(.leading, 54)
                    }
                }
                .surfaceCard()
                .padding(.horizontal, 24)
                .padding(.bottom, 24)
            }
            .overlay { if store.rows.isEmpty { ProgressView() } }
        }
        .task {
            while !Task.isCancelled {
                await store.refresh()
                try? await Task.sleep(for: .seconds(2))
            }
        }
    }

    private func appBundle(_ path: String) -> String? {
        guard let range = path.range(of: ".app/") else { return nil }
        return String(path[..<range.lowerBound]) + ".app"
    }
}
