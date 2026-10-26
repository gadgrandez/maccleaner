import SwiftUI

// Selection shared by every list that acts on the checked items: one model (a `Set` of ids),
// bulk selection for a page or a group, and the summary of what is checked.

/// How many of a group of items are checked.
enum CheckState {
    case none, some, all
}

extension Set {
    /// Inserts the element if absent; removes it if present.
    mutating func toggle(_ element: Element) {
        if contains(element) { remove(element) } else { insert(element) }
    }

    func checkState(of ids: some Collection<Element>) -> CheckState {
        let checked = ids.filter(contains).count
        return checked == 0 ? .none : checked == ids.count ? .all : .some
    }
}

/// Checks or unchecks a group of items at once (what a page or a group shows).
struct BulkSelection {
    let state: CheckState
    /// Checks every item (`true`) or none (`false`).
    let select: (Bool) -> Void
    /// Nothing to check: the control is disabled.
    var isEmpty = false

    /// Checks everything unless it already is; then unchecks everything.
    func toggle() { select(state != .all) }

    /// `ids` inside `selection`.
    init<ID: Hashable>(_ ids: some Collection<ID>, in selection: Binding<Set<ID>>) {
        let ids = Array(ids)
        state = ids.isEmpty ? .none : selection.wrappedValue.checkState(of: ids)
        isEmpty = ids.isEmpty
        select = { on in
            if on { selection.wrappedValue.formUnion(ids) } else { selection.wrappedValue.subtract(ids) }
        }
    }

    private init(state: CheckState, isEmpty: Bool, select: @escaping (Bool) -> Void) {
        self.state = state
        self.isEmpty = isEmpty
        self.select = select
    }

    /// Several groups (or selections) handled as one: a page made of groups.
    init(combining parts: [BulkSelection]) {
        let parts = parts.filter { !$0.isEmpty }
        let state: CheckState = if parts.allSatisfy({ $0.state == .all }) && !parts.isEmpty {
            .all
        } else if parts.allSatisfy({ $0.state == .none }) {
            .none
        } else {
            .some
        }
        self.init(state: state, isEmpty: parts.isEmpty) { on in parts.forEach { $0.select(on) } }
    }
}

/// Checkbox for a `BulkSelection`; mixed while only some items are checked.
struct SelectAllToggle: View {
    let selection: BulkSelection
    /// Shown next to the box; otherwise only read by VoiceOver.
    var title = loc("Select all")
    var showsTitle = true

    var body: some View {
        // Two sources that differ give the native mixed state.
        let sources = [
            Binding(get: { selection.state != .none }, set: { _ in selection.toggle() }),
            Binding(get: { selection.state == .all }, set: { _ in }),
        ]
        Toggle(sources: sources, isOn: \.self) {
            Text(title)
        }
        .toggleStyle(.checkbox)
        .modifier(HiddenLabel(hidden: !showsTitle))
        .disabled(selection.isEmpty)
        .help(selection.state == .all ? loc("Deselect all") : title)
    }
}

private struct HiddenLabel: ViewModifier {
    let hidden: Bool

    func body(content: Content) -> some View {
        if hidden { content.labelsHidden() } else { content }
    }
}

/// What is checked, for the action bar: number of items and, when known, their size.
struct SelectionSummary: View {
    let count: Int
    var bytes: UInt64? = nil

    var body: some View {
        let items = locCount(count, one: { loc("\($0) item selected") }, other: { loc("\($0) items selected") })
        Text(count == 0 ? loc("Nothing selected") : bytes.map { "\(items) · \(formatBytes($0))" } ?? items)
            .font(.headline).monospacedDigit()
    }
}
