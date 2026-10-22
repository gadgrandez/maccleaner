import SwiftUI

@MainActor
@Observable
final class DevStore {
    static let inactiveDays: UInt32 = 30

    private let alerts: Alerts
    var projects: DevProjectsReport?
    var tools: ToolchainReport?
    var scanning = false
    var cleaning = false
    var confirming = false
    var inactiveOnly = false
    let projectControls = ListControls<DevProject>([
        .size(\.bytes),
        .number("inactivity", loc("Inactivity"), symbol: "clock") { $0.inactiveDays },
        .name(\.name),
        .text("path", loc("Location"), symbol: "folder", \.path),
    ], searchPrompt: loc("Search projects")) { [$0.name, $0.path] + $0.ecosystems }
    let toolControls = ListControls<ToolCache>([.size(\.bytes), .name { locCore($0.name) }]) { [locCore($0.name), $0.tool] }
    var selectedArtifacts: Set<String> = []
    var selectedTools: Set<String> = []
    var lastClean: CleanReport?
    var lastCommands: [CommandResult] = []

    private let slot: ScanSlot

    init(alerts: Alerts, scans: ScanCoordinator) {
        self.alerts = alerts
        slot = ScanSlot(loc("Developer Junk"), coordinator: scans)
    }

    /// Queued, waiting for another scan to finish.
    var isWaiting: Bool { slot.isWaiting }

    var hasResults: Bool { projects != nil || tools != nil }

    var visibleProjects: [DevProject] {
        let all = projects?.projects ?? []
        return projectControls.apply(inactiveOnly ? all.filter { $0.inactiveDays >= Self.inactiveDays } : all)
    }

    /// Tools share the header search and are sorted by size or name.
    var visibleTools: [ToolCache] {
        let q = projectControls.search.trimmingCharacters(in: .whitespaces)
        let items = (tools?.items ?? []).filter { q.isEmpty || locCore($0.name).localizedCaseInsensitiveContains(q) || $0.tool.localizedCaseInsensitiveContains(q) }
        return toolControls.sorted(items)
    }

    var inactiveBytes: UInt64 {
        projects?.projects.filter { $0.inactiveDays >= Self.inactiveDays }.reduce(0) { $0 + $1.bytes } ?? 0
    }

    var selectedBytes: UInt64 {
        let artifacts = projects?.projects.flatMap(\.artifacts).filter { selectedArtifacts.contains($0.path) }.reduce(0) { $0 + $1.bytes } ?? 0
        let toolBytes = tools?.items.filter { selectedTools.contains($0.id) }.reduce(0) { $0 + $1.bytes } ?? 0
        return artifacts + toolBytes
    }

    var selectedCommandCount: Int {
        tools?.items.filter { $0.action == .command && selectedTools.contains($0.id) }.count ?? 0
    }

    /// Scans projects and tools in order (not in parallel), like Smart Care.
    func scan() async {
        await slot.run { [self] in
            scanning = true
            defer { scanning = false }
            guard let p = await alerts.run({ try await Core.devProjects() }) else { return }
            guard let t = await alerts.run({ try await Core.devToolchains() }) else { return }
            projects = p
            tools = t
            selectedArtifacts = Set(p.projects.filter { $0.inactiveDays >= Self.inactiveDays }.flatMap { $0.artifacts.map(\.path) })
            selectedTools = Set(t.items.filter(\.defaultSelected).map(\.id))
        }
    }

    func isSelected(_ project: DevProject) -> Bool {
        project.artifacts.allSatisfy { selectedArtifacts.contains($0.path) }
    }

    func toggle(_ project: DevProject) {
        let paths = project.artifacts.map(\.path)
        if isSelected(project) { selectedArtifacts.subtract(paths) } else { selectedArtifacts.formUnion(paths) }
    }

    func toggle(artifact path: String) { selectedArtifacts.toggle(path) }

    func toggle(tool id: String) { selectedTools.toggle(id) }

    func clean() async {
        let items = tools?.items ?? []
        let paths = Array(selectedArtifacts)
            + items.filter { $0.action == .trash && selectedTools.contains($0.id) }.flatMap(\.paths)
        let commands = items.filter { $0.action == .command && selectedTools.contains($0.id) }.map(\.id)
        cleaning = true
        defer { cleaning = false }
        if !paths.isEmpty {
            guard let r = await alerts.run({ try await Core.devClean(paths: paths) }) else { return }
            lastClean = r
        }
        if !commands.isEmpty {
            lastCommands = await alerts.run({ try await Core.devRun(ids: commands) }) ?? []
            if let failed = lastCommands.first(where: { !$0.ok }) {
                alerts.message = "\(failed.id): \(failed.error.map(locCore) ?? loc("failed"))"
            }
        }
        await scan()
    }
}

struct DevView: View {
    @Environment(AppState.self) private var state
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let store = state.dev
        @Bindable var bindable = store
        ModulePage(.devJunk, subtitle: loc("Project dependencies and builds, toolchain caches and old versions.")) {
            if store.hasResults {
                Toggle(loc("Inactive only"), isOn: $bindable.inactiveOnly).toggleStyle(.switch).controlSize(.small)
                ListToolbar(controls: store.projectControls, busy: store.scanning || store.cleaning || store.isWaiting) { Task { await store.scan() } }
            }
        } content: {
            if !store.hasResults {
                HeroScan(theme: Destination.devJunk.theme, symbol: "hammer.fill", title: loc("Find developer junk"),
                         message: loc("node_modules, target, Pods, .venv, .gradle, .next… only next to the file that generates them. Plus npm, Bun, Cargo, Gradle and uv caches and old Node or Rust versions. With Full Disk Access it also checks Desktop, Documents and Downloads."),
                         busy: store.scanning, waiting: store.isWaiting) { Task { await store.scan() } }
            } else {
                ZStack(alignment: .bottom) {
                    ScrollView {
                        LazyVStack(spacing: Space.m) {
                            summary(store)
                            if !store.visibleProjects.isEmpty {
                                sectionTitle(loc("Projects"), trailing: loc("\(store.visibleProjects.count.formatted()) · by \(store.projectControls.option.title.lowercased())"))
                                ForEach(Array(store.visibleProjects.enumerated()), id: \.element.id) { i, project in
                                    ProjectCard(store: store, project: project)
                                        .transition(Motion.appear(i, reduceMotion: reduceMotion))
                                }
                            }
                            if let tools = store.tools, !store.visibleTools.isEmpty {
                                sectionTitle(loc("Tools"), trailing: formatBytes(tools.totalBytes))
                                CardSection(title: loc("Caches and old versions")) {
                                    ForEach(store.visibleTools) { item in toolRow(item, store: store) }
                                }
                            }
                        }
                        .padding(.horizontal, Space.xl)
                        .padding(.bottom, 96)
                        .animation(Motion.standard, value: store.projects?.elapsedMs)
                    }
                    ActionBar {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(loc("\(formatBytes(store.selectedBytes)) selected")).font(.headline).monospacedDigit()
                            if let last = store.lastClean {
                                LastCleanSummary(report: last, cleanedBytes: last.trashedBytes)
                            }
                        }
                    } trailing: {
                        Button { store.confirming = true } label: {
                            Label(store.cleaning ? loc("Cleaning…") : loc("Clean"), systemImage: "sparkles").frame(minWidth: 110)
                        }
                        .primaryActionStyle().controlSize(.large)
                        .disabled(store.selectedBytes == 0 || store.cleaning || store.scanning)
                    }
                }
            }
        }
        .confirmationDialog(loc("Clean \(formatBytes(store.selectedBytes))?"), isPresented: $bindable.confirming) {
            Button(loc("Clean"), role: .destructive) { Task { await store.clean() } }
        } message: {
            Text(store.selectedCommandCount > 0
                 ? loc("Folders will go to the Trash and \(store.selectedCommandCount.formatted()) command cleanups (Homebrew/Docker) will run, which can't be undone. Dependencies are regenerated with npm install, cargo build, pod install…")
                 : loc("Folders will go to the Trash. Dependencies are regenerated with npm install, cargo build, pod install…"))
        }
    }

    private func summary(_ store: DevStore) -> some View {
        HStack(spacing: Space.m) {
            StatTile(symbol: "folder.fill", theme: Destination.devJunk.theme,
                     value: formatBytes(store.projects?.totalBytes ?? 0), label: loc("In projects"),
                     detail: loc("\(formatBytes(store.inactiveBytes)) in inactive ones"))
            StatTile(symbol: "shippingbox.fill", theme: ModuleTheme(colors: [.orange, .pink]),
                     value: formatBytes(store.tools?.totalBytes ?? 0), label: loc("In tools"))
            StatTile(symbol: "checkmark.circle.fill", theme: ModuleTheme(colors: [.blue, .teal]),
                     value: formatBytes(store.selectedBytes), label: loc("Selected"))
        }
    }

    private func sectionTitle(_ title: String, trailing: String) -> some View {
        HStack {
            Text(title).font(.title3.weight(.semibold)).accessibilityAddTraits(.isHeader)
            Spacer()
            Text(trailing).foregroundStyle(.secondary).monospacedDigit()
        }
        .padding(.top, Space.s)
    }

    private func toolRow(_ item: ToolCache, store: DevStore) -> some View {
        SelectableRow(isOn: store.selectedTools.contains(item.id), title: locCore(item.name),
                      subtitle: item.note.map(locCore) ?? locCore(item.tool), bytes: item.bytes,
                      path: item.paths.first) {
            Image(systemName: item.action == .command ? "terminal" : "shippingbox")
                .frame(width: 22).foregroundStyle(Destination.devJunk.theme.tint)
        } toggle: {
            store.toggle(tool: item.id)
        }
    }
}

private struct ProjectCard: View {
    let store: DevStore
    let project: DevProject

    var body: some View {
        let inactive = project.inactiveDays >= DevStore.inactiveDays
        VStack(alignment: .leading, spacing: Space.s) {
            HStack(alignment: .firstTextBaseline, spacing: Space.m) {
                Toggle(project.name, isOn: Binding(get: { store.isSelected(project) }, set: { _ in store.toggle(project) }))
                    .toggleStyle(.checkbox).labelsHidden()
                VStack(alignment: .leading, spacing: 3) {
                    HStack(spacing: 6) {
                        Text(project.name).font(.headline)
                        ForEach(project.ecosystems, id: \.self) { Pill(text: $0, color: Destination.devJunk.theme.tint) }
                        Pill(text: inactive ? locCount(project.inactiveDays, one: { loc("inactive \($0) day") }, other: { loc("inactive \($0) days") }) : activity(project.inactiveDays),
                             color: inactive ? .orange : .green)
                    }
                    Text(abbreviatingHome(project.path)).font(.caption).foregroundStyle(.secondary)
                        .lineLimit(1).truncationMode(.head).help(project.path)
                }
                Spacer(minLength: Space.s)
                Text(formatBytes(project.bytes)).font(.title3.weight(.semibold)).monospacedDigit()
            }
            ForEach(project.artifacts) { artifact in
                SelectableRow(isOn: store.selectedArtifacts.contains(artifact.path), title: artifact.kind,
                              subtitle: artifact.ecosystem, bytes: artifact.bytes, path: artifact.path) {
                    Image(systemName: "folder.fill").frame(width: 22).foregroundStyle(.secondary)
                } toggle: {
                    store.toggle(artifact: artifact.path)
                }
                .padding(.leading, 26)
            }
        }
        .padding(Space.l)
        .surfaceCard()
    }

    private func activity(_ days: UInt32) -> String {
        switch days {
        case 0: loc("active today")
        case 1: loc("active yesterday")
        default: locCount(days, one: { loc("active \($0) day ago") }, other: { loc("active \($0) days ago") })
        }
    }
}
