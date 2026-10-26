import SwiftUI

// MARK: Space Lens

@MainActor
@Observable
final class SpaceLensStore {
    private let alerts: Alerts
    var tree: SpaceNode?
    var loading = false
    var history: [String] = []
    var hovered: SpaceNode?
    let controls = ListControls<SpaceNode>([
        .size(\.bytes),
        .name(\.name),
        .number("files", loc("Files"), symbol: "doc.on.doc") { $0.files },
    ], searchPrompt: loc("Search")) { [$0.name] }

    private let slot: ScanSlot

    init(alerts: Alerts, scans: ScanCoordinator) {
        self.alerts = alerts
        slot = ScanSlot(loc("Space Lens"), coordinator: scans)
    }

    /// Queued, waiting for another scan to finish.
    var isWaiting: Bool { slot.isWaiting }

    func load(_ path: String, push: Bool = true) async {
        await slot.run { [self] in
            if push, let current = tree?.path, current != path { history.append(current) }
            loading = true
            defer { loading = false }
            hovered = nil
            if let t = await alerts.run({ try await Core.spaceTree(path: path, depth: 2, top: 12) }) { tree = t }
        }
    }

    func back() async {
        guard let previous = history.popLast() else { return }
        await load(previous, push: false)
    }

    func choose() async {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        if panel.runModal() == .OK, let url = panel.url {
            history = []
            await load(url.path, push: false)
        }
    }
}

private let lensPalette: [Color] = [.purple, .blue, .teal, .green, .yellow, .orange, .pink, .indigo, .cyan, .mint, .red, .brown]

struct SpaceLensView: View {
    @Environment(AppState.self) private var state

    var body: some View {
        let store = state.spaceLens
        ModulePage(.spaceLens, subtitle: loc("Interactive map of what takes up space. Click a segment to go into it.")) {
            if store.tree != nil { ListToolbar(controls: store.controls) }
            IconButton(symbol: "chevron.left", label: loc("Back"), disabled: store.history.isEmpty || store.loading) { Task { await store.back() } }
            Button { Task { await store.choose() } } label: { Label(loc("Choose folder"), systemImage: "folder") }
                .secondaryActionStyle().disabled(store.loading)
        } content: {
            if let tree = store.tree {
                HStack(alignment: .top, spacing: 18) {
                    Sunburst(root: tree, store: store)
                        .aspectRatio(1, contentMode: .fit)
                        .frame(maxWidth: 520)
                        .padding(12)
                        .opacity(store.loading ? 0.4 : 1)
                        .overlay { if store.loading { ProgressView().controlSize(.large) } }
                    childList(tree, store: store)
                }
                .padding(.horizontal, 24).padding(.bottom, 20)
            } else {
                HeroScan(theme: Destination.spaceLens.theme, symbol: "circle.hexagongrid.fill", title: loc("Explore your home folder"),
                         message: loc("Works out what takes up space and shows it as an interactive map. You can choose another folder above."),
                         buttonTitle: loc("Analyze"), busy: store.loading, busyText: loc("Calculating…"), waiting: store.isWaiting) {
                    Task { await store.load(userHome, push: false) }
                }
            }
        }
    }

    private func childList(_ tree: SpaceNode, store: SpaceLensStore) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(verbatim: abbreviatingHome(tree.path)).font(.headline).lineLimit(1).truncationMode(.head).help(tree.path)
            Text(loc("\(formatBytes(tree.bytes)) · \(countFiles(tree.files))")).font(.callout).foregroundStyle(.secondary)
            ScrollView {
                LazyVStack(spacing: 2) {
                    // The color identifies the chart segment, so it comes from the original order.
                    let colorIndex = Dictionary(uniqueKeysWithValues: tree.children.enumerated().map { ($1.path, $0) })
                    ForEach(store.controls.apply(tree.children)) { child in
                        HStack(spacing: 10) {
                            RoundedRectangle(cornerRadius: 3)
                                .fill(lensPalette[(colorIndex[child.path] ?? 0) % lensPalette.count])
                                .frame(width: 10, height: 10)
                            Image(systemName: child.isDir ? "folder.fill" : "doc.fill").foregroundStyle(.secondary)
                            Text(child.name).lineLimit(1).truncationMode(.middle)
                            Spacer()
                            Text(formatBytes(child.bytes)).monospacedDigit().foregroundStyle(.secondary)
                            RevealButton(path: child.path)
                        }
                        .padding(.horizontal, 10).padding(.vertical, 7)
                        .hoverHighlight(selected: store.hovered?.path == child.path)
                        .onTapGesture { if child.isDir { Task { await store.load(child.path) } } }
                        .onHover { inside in store.hovered = inside ? child : nil }
                    }
                    if tree.othersBytes > 0 {
                        HStack {
                            Text(loc("Other items")).foregroundStyle(.secondary)
                            Spacer()
                            Text(formatBytes(tree.othersBytes)).monospacedDigit().foregroundStyle(.secondary)
                        }
                        .padding(.horizontal, 10).padding(.vertical, 7)
                    }
                }
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .surfaceCard()
    }
}

/// Two-ring sunburst chart drawn with custom shapes (allows hover and click per segment).
private struct Sunburst: View {
    let root: SpaceNode
    let store: SpaceLensStore

    private struct Segment: Identifiable {
        let id: String
        let node: SpaceNode?
        let start: Double
        let end: Double
        let ring: Int
        let color: Color
    }

    private var segments: [Segment] {
        let total = Double(max(root.bytes, 1))
        var result: [Segment] = []
        var cursor = 0.0
        for (i, child) in root.children.enumerated() {
            let color = lensPalette[i % lensPalette.count]
            let span = Double(child.bytes) / total
            result.append(Segment(id: child.path, node: child, start: cursor, end: cursor + span, ring: 0, color: color))
            var inner = cursor
            let childTotal = Double(max(child.bytes, 1))
            for (j, grandchild) in child.children.enumerated() {
                let gSpan = span * Double(grandchild.bytes) / childTotal
                let opacity = max(0.35, 0.9 - Double(j) * 0.06)
                result.append(Segment(id: grandchild.path, node: grandchild, start: inner, end: inner + gSpan, ring: 1, color: color.opacity(opacity)))
                inner += gSpan
            }
            cursor += span
        }
        return result.filter { $0.end - $0.start > 0.002 }
    }

    var body: some View {
        GeometryReader { geo in
            let size = min(geo.size.width, geo.size.height)
            ZStack {
                ForEach(segments) { seg in
                    let hovered = store.hovered?.path == seg.id
                    AnnularSector(start: seg.start, end: seg.end,
                                  inner: seg.ring == 0 ? 0.30 : 0.64,
                                  outer: seg.ring == 0 ? 0.62 : (hovered ? 1.0 : 0.96))
                        .fill(seg.color)
                        .overlay(AnnularSector(start: seg.start, end: seg.end,
                                               inner: seg.ring == 0 ? 0.30 : 0.64,
                                               outer: seg.ring == 0 ? 0.62 : (hovered ? 1.0 : 0.96))
                            .stroke(Color(nsColor: .windowBackgroundColor), lineWidth: 1.5))
                        .brightness(hovered ? 0.12 : 0)
                        .onHover { inside in
                            if inside { store.hovered = seg.node } else if store.hovered?.path == seg.id { store.hovered = nil }
                        }
                        .onTapGesture {
                            if let node = seg.node, node.isDir { Task { await store.load(node.path) } }
                        }
                }
                VStack(spacing: 4) {
                    let focus = store.hovered ?? root
                    Image(systemName: focus.isDir ? "folder.fill" : "doc.fill").font(.title2).foregroundStyle(.secondary)
                    Text(focus.name).font(.headline).lineLimit(2).multilineTextAlignment(.center)
                    Text(formatBytes(focus.bytes)).font(.title2.weight(.bold)).monospacedDigit()
                }
                .frame(width: size * 0.5)
                .allowsHitTesting(false)
            }
            .frame(width: size, height: size)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .animation(.snappy(duration: 0.2), value: store.hovered?.path)
        }
    }
}

/// Annular sector: `start`/`end` as a fraction of a turn, radii as a fraction of the maximum radius.
struct AnnularSector: Shape {
    var start: Double
    var end: Double
    var inner: Double
    var outer: Double

    var animatableData: AnimatablePair<Double, Double> {
        get { AnimatablePair(inner, outer) }
        set { inner = newValue.first; outer = newValue.second }
    }

    func path(in rect: CGRect) -> Path {
        let center = CGPoint(x: rect.midX, y: rect.midY)
        let radius = min(rect.width, rect.height) / 2
        let a0 = Angle.degrees(start * 360 - 90), a1 = Angle.degrees(end * 360 - 90)
        var p = Path()
        p.addArc(center: center, radius: radius * outer, startAngle: a0, endAngle: a1, clockwise: false)
        p.addArc(center: center, radius: radius * inner, startAngle: a1, endAngle: a0, clockwise: true)
        p.closeSubpath()
        return p
    }
}

// MARK: Large & old files

@MainActor
@Observable
final class LargeFilesStore {
    private let alerts: Alerts
    var report: LargeFilesReport?
    var scanning = false
    var minBytes: UInt64 = 100_000_000
    var olderThanDays: UInt32? = nil
    var kind: String? = nil
    var selection = Set<String>()
    var trashing = false
    var confirming = false
    let controls = ListControls<FileInfo>([
        .size(\.bytes),
        .number("modified", loc("Modified"), symbol: "calendar", ascending: true) { $0.modified },
        .number("accessed", loc("Last opened"), symbol: "eye", ascending: true) { $0.accessed },
        .name(\.name),
    ], searchPrompt: loc("Search files")) { [$0.name, $0.path] }

    private let slot: ScanSlot

    init(alerts: Alerts, scans: ScanCoordinator) {
        self.alerts = alerts
        slot = ScanSlot(loc("Large & Old Files"), coordinator: scans)
    }

    /// Queued, waiting for another scan to finish.
    var isWaiting: Bool { slot.isWaiting }

    var files: [FileInfo] {
        guard let r = report else { return [] }
        return controls.apply(kind.map { k in r.files.filter { $0.kind == k } } ?? r.files)
    }

    var kinds: [String] { Array(Set(report?.files.map(\.kind) ?? [])).sorted() }

    /// Every file on screen.
    var selectAll: BulkSelection { BulkSelection(files.map(\.path), in: Bindable(self).selection) }

    var selectedBytes: UInt64 { report?.files.filter { selection.contains($0.path) }.reduce(0) { $0 + $1.bytes } ?? 0 }

    func scan() async {
        await slot.run { [self] in
            scanning = true
            defer { scanning = false }
            let request = LargeFilesRequest(minBytes: minBytes, olderThanDays: olderThanDays)
            guard let r = await alerts.run({ try await Core.largeFiles(request) }) else { return }
            report = r
            selection.removeAll()
            kind = nil
        }
    }

    func trash() async {
        trashing = true
        defer { trashing = false }
        let paths = Array(selection)
        guard await alerts.run({ try await Core.trash(paths: paths) }) != nil else { return }
        await scan()
    }
}

struct LargeFilesView: View {
    @Environment(AppState.self) private var state

    var body: some View {
        let store = state.largeFiles
        @Bindable var bindable = store
        ModulePage(.largeFiles, subtitle: loc("Heavy or forgotten files in your home folder.")) {
            Picker(loc("Size"), selection: $bindable.minBytes) {
                Text(loc("> 50 MB")).tag(UInt64(50_000_000))
                Text(loc("> 100 MB")).tag(UInt64(100_000_000))
                Text(loc("> 500 MB")).tag(UInt64(500_000_000))
                Text(loc("> 1 GB")).tag(UInt64(1_000_000_000))
            }.pickerStyle(.menu).fixedSize()
            Picker(loc("Age"), selection: $bindable.olderThanDays) {
                Text(loc("Any")).tag(UInt32?.none)
                Text(loc("> 1 month")).tag(UInt32?.some(30))
                Text(loc("> 6 months")).tag(UInt32?.some(180))
                Text(loc("> 1 year")).tag(UInt32?.some(365))
            }.pickerStyle(.menu).fixedSize()
            if store.report != nil {
                ListToolbar(controls: store.controls, busy: store.scanning || store.isWaiting, disabled: store.trashing,
                            selectAll: store.selectAll) { Task { await store.scan() } }
            }
        } content: {
            if store.report == nil {
                HeroScan(theme: Destination.largeFiles.theme, symbol: "doc.badge.clock.fill", title: loc("Find large and old files"),
                         message: loc("Videos, disk images and forgotten downloads. Adjust size and age above."),
                         busy: store.scanning, waiting: store.isWaiting) { Task { await store.scan() } }
            } else if store.report?.files.isEmpty == true {
                EmptyResult(symbol: "checkmark.seal.fill", title: loc("Nothing to review"), message: loc("No files match those criteria."))
            } else {
                ZStack(alignment: .bottom) {
                    VStack(spacing: 10) {
                        kindFilter(store)
                        ScrollView {
                            LazyVStack(spacing: 0) {
                                ForEach(store.files) { file in
                                    SelectableRow(isOn: store.selection.contains(file.path), title: file.name,
                                                  subtitle: loc("\(abbreviatingHome((file.path as NSString).deletingLastPathComponent)) · modified \(formatDate(file.modified))"),
                                                  bytes: file.bytes, path: file.path) {
                                        FileIcon(path: file.path, size: 30)
                                    } toggle: { store.selection.toggle(file.path) }
                                    .padding(.horizontal, 14)
                                }
                            }
                            .padding(.vertical, 6)
                            .surfaceCard()
                            .padding(.bottom, 96)
                        }
                    }
                    .padding(.horizontal, 24)
                    ActionBar {
                        if store.trashing {
                            CleanProgressView(title: state.preferences.removingTitle)
                        } else {
                            SelectionSummary(count: store.selection.count, bytes: store.selectedBytes)
                        }
                    } trailing: {
                        Button { store.confirming = true } label: { Label(state.preferences.removeTitle, systemImage: "trash") }
                            .primaryActionStyle().controlSize(.large)
                            .disabled(store.selection.isEmpty || store.trashing)
                    }
                }
            }
        }
        .confirmationDialog(state.preferences.deletePermanently
                            ? loc("Delete \(countFiles(store.selection.count)) permanently?")
                            : loc("Move \(countFiles(store.selection.count)) to the Trash?"), isPresented: $bindable.confirming) {
            Button(state.preferences.removeTitle, role: .destructive) { Task { await store.trash() } }
        }
    }

    private func kindFilter(_ store: LargeFilesStore) -> some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                chip(loc("All"), active: store.kind == nil) { store.kind = nil }
                ForEach(store.kinds, id: \.self) { k in
                    let title = store.report?.files.first { $0.kind == k }?.kindTitle ?? k
                    chip(title, active: store.kind == k) { store.kind = k }
                }
            }
        }
    }

    private func chip(_ title: String, active: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(title).font(.callout.weight(.medium)).padding(.horizontal, 12).padding(.vertical, 5)
                .background(active ? Color.accentColor.opacity(0.22) : Color.secondary.opacity(0.1), in: Capsule())
        }
        .buttonStyle(.plain)
    }
}

// MARK: Duplicates

@MainActor
@Observable
final class DuplicatesStore {
    private let alerts: Alerts
    var report: DuplicatesReport?
    var scanning = false
    var selection = Set<String>()
    var trashing = false
    var confirming = false
    let controls = ListControls<DupGroup>([
        .number("wasted", loc("Duplicated space"), symbol: "externaldrive") { $0.wastedBytes },
        .size(\.bytes),
        .number("copies", loc("Copies"), symbol: "doc.on.doc") { $0.paths.count },
        .name { ($0.paths.first.map { ($0 as NSString).lastPathComponent }) ?? "" },
    ], searchPrompt: loc("Search files")) { $0.paths }

    private let slot: ScanSlot

    init(alerts: Alerts, scans: ScanCoordinator) {
        self.alerts = alerts
        slot = ScanSlot(loc("Duplicates"), coordinator: scans)
    }

    /// Queued, waiting for another scan to finish.
    var isWaiting: Bool { slot.isWaiting }

    var selectedBytes: UInt64 {
        report?.groups.reduce(0) { sum, g in sum + UInt64(g.paths.filter(selection.contains).count) * g.bytes } ?? 0
    }

    func scan() async {
        await slot.run { [self] in
            scanning = true
            defer { scanning = false }
            guard let r = await alerts.run({ try await Core.duplicates(DuplicatesRequest()) }) else { return }
            report = r
            // Keep the first copy of each group and check the rest.
            selection = Set(r.groups.flatMap { $0.paths.dropFirst() })
        }
    }

    /// Never allows checking every copy in a group.
    func toggle(_ path: String, in group: DupGroup) {
        if !selection.contains(path) && group.paths.filter({ $0 != path }).allSatisfy(selection.contains) { return }
        selection.toggle(path)
    }

    /// Every copy of a group but one: the first unchecked one is kept.
    func selectAll(in group: DupGroup) -> BulkSelection {
        let kept = group.paths.first { !selection.contains($0) } ?? group.paths.first
        return BulkSelection(group.paths.filter { $0 != kept }, in: Bindable(self).selection)
    }

    var visibleGroups: [DupGroup] { controls.apply(report?.groups ?? []) }

    /// Every group on screen.
    var selectAll: BulkSelection { BulkSelection(combining: visibleGroups.map(selectAll(in:))) }

    func trash() async {
        trashing = true
        defer { trashing = false }
        let paths = Array(selection)
        guard await alerts.run({ try await Core.trash(paths: paths) }) != nil else { return }
        await scan()
    }
}

struct DuplicatesView: View {
    @Environment(AppState.self) private var state

    var body: some View {
        let store = state.duplicates
        @Bindable var bindable = store
        ModulePage(.duplicates, subtitle: loc("Byte-for-byte identical copies. At least one is always kept.")) {
            if store.report != nil {
                ListToolbar(controls: store.controls, busy: store.scanning || store.isWaiting, disabled: store.trashing,
                            selectAll: store.selectAll) { Task { await store.scan() } }
            }
        } content: {
            if store.report == nil {
                HeroScan(theme: Destination.duplicates.theme, symbol: "doc.on.doc.fill", title: loc("Find duplicate files"),
                         message: loc("Compares size and content in your home folder (files over 1 MB)."),
                         busy: store.scanning, waiting: store.isWaiting) { Task { await store.scan() } }
            } else if store.report?.groups.isEmpty == true {
                EmptyResult(symbol: "checkmark.seal.fill", title: loc("No duplicates"), message: loc("There are no identical copies."))
            } else if let r = store.report {
                ZStack(alignment: .bottom) {
                    ScrollView {
                        LazyVStack(spacing: 12) {
                            HStack(spacing: 12) {
                                StatTile(symbol: "doc.on.doc.fill", theme: Destination.duplicates.theme, value: "\(r.groups.count)", label: loc("Groups"))
                                StatTile(symbol: "externaldrive.fill", theme: ModuleTheme(colors: [.orange, .red]), value: formatBytes(r.totalWasted), label: loc("Duplicated space"))
                            }
                            ForEach(store.visibleGroups) { group in
                                CardSection(title: (group.paths.first.map { ($0 as NSString).lastPathComponent } ?? ""),
                                            trailing: "\(group.paths.count) × \(formatBytes(group.bytes))",
                                            selectAll: store.selectAll(in: group)) {
                                    ForEach(group.paths, id: \.self) { path in
                                        SelectableRow(isOn: store.selection.contains(path), title: abbreviatingHome((path as NSString).deletingLastPathComponent),
                                                      bytes: nil, path: path) {
                                            FileIcon(path: path, size: 22)
                                        } toggle: { store.toggle(path, in: group) }
                                    }
                                }
                            }
                        }
                        .padding(.horizontal, 24).padding(.bottom, 96)
                    }
                    ActionBar {
                        if store.trashing {
                            CleanProgressView(title: state.preferences.removingTitle)
                        } else {
                            SelectionSummary(count: store.selection.count, bytes: store.selectedBytes)
                        }
                    } trailing: {
                        Button { store.confirming = true } label: { Label(state.preferences.removeTitle, systemImage: "trash") }
                            .primaryActionStyle().controlSize(.large)
                            .disabled(store.selection.isEmpty || store.trashing)
                    }
                }
            }
        }
        .confirmationDialog(state.preferences.deletePermanently
                            ? loc("Delete \(countCopies(store.selection.count)) permanently?")
                            : loc("Move \(countCopies(store.selection.count)) to the Trash?"), isPresented: $bindable.confirming) {
            Button(state.preferences.removeTitle, role: .destructive) { Task { await store.trash() } }
        }
    }
}
