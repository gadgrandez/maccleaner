import SwiftUI

enum DestinationGroup: CaseIterable {
    case main, cleanup, performance, applications, files

    var title: String? {
        switch self {
        case .main: nil
        case .cleanup: loc("Cleanup")
        case .performance: loc("Performance")
        case .applications: loc("Applications")
        case .files: loc("Files")
        }
    }

    var destinations: [Destination] { Destination.allCases.filter { $0.group == self } }
}

enum Destination: String, CaseIterable, Identifiable, Hashable {
    case smartCare, junk, devJunk, privacy
    case status, maintenance, launchItems, processes
    case uninstaller, updates, leftovers, extensions
    case spaceLens, largeFiles, duplicates

    var id: String { rawValue }

    var group: DestinationGroup {
        switch self {
        case .smartCare: .main
        case .junk, .devJunk, .privacy: .cleanup
        case .status, .maintenance, .launchItems, .processes: .performance
        case .uninstaller, .updates, .leftovers, .extensions: .applications
        case .spaceLens, .largeFiles, .duplicates: .files
        }
    }

    var title: String {
        switch self {
        case .smartCare: loc("Smart Care")
        case .junk: loc("System Junk")
        case .devJunk: loc("Developer Junk")
        case .privacy: loc("Privacy")
        case .status: loc("Mac Status")
        case .maintenance: loc("Maintenance")
        case .launchItems: loc("Login & Background")
        case .processes: loc("Processes")
        case .uninstaller: loc("Uninstaller")
        case .updates: loc("Updates")
        case .leftovers: loc("App Leftovers")
        case .extensions: loc("Extensions")
        case .spaceLens: loc("Space Lens")
        case .largeFiles: loc("Large & Old Files")
        case .duplicates: loc("Duplicates")
        }
    }

    var symbol: String {
        switch self {
        case .smartCare: "sparkles"
        case .junk: "trash.fill"
        case .devJunk: "hammer.fill"
        case .privacy: "hand.raised.fill"
        case .status: "gauge.with.dots.needle.67percent"
        case .maintenance: "wrench.and.screwdriver.fill"
        case .launchItems: "power"
        case .processes: "cpu.fill"
        case .uninstaller: "xmark.app.fill"
        case .updates: "arrow.down.app.fill"
        case .leftovers: "shippingbox.fill"
        case .extensions: "puzzlepiece.extension.fill"
        case .spaceLens: "circle.hexagongrid.fill"
        case .largeFiles: "doc.badge.clock.fill"
        case .duplicates: "doc.on.doc.fill"
        }
    }

    var theme: ModuleTheme {
        switch self {
        case .smartCare: ModuleTheme(colors: [.purple, .indigo])
        case .junk: ModuleTheme(colors: [.green, .teal])
        case .devJunk: ModuleTheme(colors: [.indigo, .cyan])
        case .privacy: ModuleTheme(colors: [.blue, .cyan])
        case .status: ModuleTheme(colors: [.teal, .blue])
        case .maintenance: ModuleTheme(colors: [.orange, .yellow])
        case .launchItems: ModuleTheme(colors: [.orange, .red])
        case .processes: ModuleTheme(colors: [.red, .pink])
        case .uninstaller: ModuleTheme(colors: [.blue, .indigo])
        case .updates: ModuleTheme(colors: [.cyan, .blue])
        case .leftovers: ModuleTheme(colors: [.indigo, .purple])
        case .extensions: ModuleTheme(colors: [.mint, .teal])
        case .spaceLens: ModuleTheme(colors: [.purple, .pink])
        case .largeFiles: ModuleTheme(colors: [.yellow, .orange])
        case .duplicates: ModuleTheme(colors: [.pink, .purple])
        }
    }
}

struct ContentView: View {
    @Environment(AppState.self) private var state

    var body: some View {
        @Bindable var state = state
        NavigationSplitView {
            List(selection: $state.destination) {
                // Inside the list (not in the footer): a footer whose height depends on the
                // sidebar width fed back into the layout in a loop.
                if state.fullDiskAccess == false {
                    Section { AccessCard(state: state) }
                }
                ForEach(DestinationGroup.allCases, id: \.self) { group in
                    Section {
                        ForEach(group.destinations) { d in
                            Label {
                                Text(d.title)
                            } icon: {
                                ModuleIcon(symbol: d.symbol, theme: d.theme, size: 22)
                            }
                            .tag(d)
                        }
                    } header: {
                        if let title = group.title { Text(title) }
                    }
                }
            }
            .listStyle(.sidebar)
            .safeAreaInset(edge: .bottom) {
                VStack(alignment: .leading, spacing: 0) {
                    SettingsLink { Label(loc("Settings"), systemImage: "gearshape") }
                        .buttonStyle(.borderless)
                        .padding(.horizontal, Space.l)
                        .padding(.vertical, Space.s)
                    DiskFooter(status: state.status.system)
                }
            }
            // Checked again when returning to the app (e.g. after granting the permission in Settings).
            .task { await state.refreshAccess() }
            .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
                Task { await state.refreshAccess() }
            }
            .navigationSplitViewColumnWidth(min: 220, ideal: 240)
            // Cheap read (statfs + sysctl) for the sidebar footer.
            .task {
                while !Task.isCancelled {
                    await state.status.refresh()
                    try? await Task.sleep(for: .seconds(60))
                }
            }
        } detail: {
            detail(for: state.destination ?? .smartCare)
        }
        .alert(loc("Couldn't complete"), isPresented: Binding(
            get: { state.alerts.message != nil },
            set: { if !$0 { state.alerts.message = nil } }
        )) {
            Button(loc("OK"), role: .cancel) {}
        } message: {
            Text(state.alerts.message ?? "")
        }
    }

    @ViewBuilder
    private func detail(for d: Destination) -> some View {
        switch d {
        case .smartCare: SmartCareView()
        case .junk: JunkView()
        case .devJunk: DevView()
        case .privacy: PrivacyView()
        case .status: StatusView()
        case .maintenance: MaintenanceView()
        case .launchItems: LaunchItemsView()
        case .processes: ProcessesView()
        case .uninstaller: UninstallerView()
        case .updates: UpdatesView()
        case .leftovers: LeftoversView()
        case .extensions: ExtensionsView()
        case .spaceLens: SpaceLensView()
        case .largeFiles: LargeFilesView()
        case .duplicates: DuplicatesView()
        }
    }
}

/// Sidebar footer: free space on the startup disk.
private struct DiskFooter: View {
    let status: SystemStatus?

    var body: some View {
        if let s = status, s.diskTotal > 0 {
            let used = Double(s.diskTotal - min(s.diskAvailable, s.diskTotal)) / Double(s.diskTotal)
            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    Label(loc("Startup disk"), systemImage: "internaldrive").font(.caption.weight(.semibold))
                    Spacer()
                }
                ProgressView(value: used)
                    .tint(used > 0.9 ? .red : (used > 0.8 ? .orange : .accentColor))
                Text(loc("\(formatBytes(s.diskAvailable)) free of \(formatBytes(s.diskTotal))"))
                    .font(.caption).foregroundStyle(.secondary).monospacedDigit()
            }
            .padding(Space.m)
            .surfaceCard(cornerRadius: Radius.row + 2)
            .padding(.horizontal, Space.s + 2)
            .padding(.bottom, Space.s + 2)
            .accessibilityElement(children: .combine)
        }
    }
}

/// Notice shown when Full Disk Access is missing: without it, protected folders aren't scanned.
private struct AccessCard: View {
    let state: AppState

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Label(loc("Limited access"), systemImage: "lock.fill")
                .font(.caption.weight(.semibold))
                .foregroundStyle(.orange)
            Text(loc("Grant Full Disk Access to scan Desktop, Documents, Downloads, the Trash and Mail without interruptions."))
                .font(.caption).foregroundStyle(.secondary)
                .lineLimit(5)
            Button(loc("Open Settings")) { state.openFullDiskAccessSettings() }
                .controlSize(.small)
                .secondaryActionStyle()
        }
        .padding(Space.m)
        .frame(maxWidth: .infinity, alignment: .leading)
        .surfaceCard(cornerRadius: Radius.row + 2)
        .selectionDisabled()
    }
}
