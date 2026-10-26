import SwiftUI

/// Sort criterion for a list. `before` defines the ascending order.
struct SortOption<Item>: Identifiable {
    let id: String
    let title: String
    let symbol: String
    /// Natural direction: largest or most recent first (descending); text from A to Z (ascending).
    let defaultAscending: Bool
    let before: (Item, Item) -> Bool

    static func size(_ value: @escaping (Item) -> UInt64) -> SortOption {
        SortOption(id: "size", title: loc("Size"), symbol: "externaldrive", defaultAscending: false) { value($0) < value($1) }
    }

    static func name(_ value: @escaping (Item) -> String, title: String = loc("Name")) -> SortOption {
        SortOption(id: "name", title: title, symbol: "textformat", defaultAscending: true) {
            value($0).localizedStandardCompare(value($1)) == .orderedAscending
        }
    }

    static func number<N: Comparable>(_ id: String, _ title: String, symbol: String, ascending: Bool = false,
                                      _ value: @escaping (Item) -> N) -> SortOption {
        SortOption(id: id, title: title, symbol: symbol, defaultAscending: ascending) { value($0) < value($1) }
    }

    static func text(_ id: String, _ title: String, symbol: String, _ value: @escaping (Item) -> String) -> SortOption {
        SortOption(id: id, title: title, symbol: symbol, defaultAscending: true) {
            value($0).localizedStandardCompare(value($1)) == .orderedAscending
        }
    }
}

/// Search, sort and direction reusable by any list.
@MainActor
@Observable
final class ListControls<Item> {
    let options: [SortOption<Item>]
    let searchPrompt: String
    private let searchable: (Item) -> [String]
    var search = ""
    var ascending: Bool
    var sortID: String {
        didSet {
            if sortID != oldValue { ascending = option.defaultAscending }
        }
    }

    init(_ options: [SortOption<Item>], searchPrompt: String = loc("Search"), searchable: @escaping (Item) -> [String]) {
        precondition(!options.isEmpty)
        self.options = options
        self.searchPrompt = searchPrompt
        self.searchable = searchable
        sortID = options[0].id
        ascending = options[0].defaultAscending
    }

    var option: SortOption<Item> { options.first { $0.id == sortID } ?? options[0] }

    var isSearching: Bool { !search.trimmingCharacters(in: .whitespaces).isEmpty }

    func matches(_ item: Item) -> Bool {
        let query = search.trimmingCharacters(in: .whitespaces)
        return query.isEmpty || searchable(item).contains { $0.localizedCaseInsensitiveContains(query) }
    }

    /// Filters by the search and sorts by the current criterion and direction.
    func apply(_ items: [Item]) -> [Item] {
        sorted(items.filter(matches))
    }

    func sorted(_ items: [Item]) -> [Item] {
        let before = option.before
        return items.sorted { ascending ? before($0, $1) : before($1, $0) }
    }
}

/// Search field + sort menu + direction for a module's header.
struct ListControlsBar<Item>: View {
    @Bindable var controls: ListControls<Item>
    var showsSearch = true

    var body: some View {
        Group {
            if showsSearch {
                TextField(controls.searchPrompt, text: $controls.search)
                    .textFieldStyle(.roundedBorder)
                    .frame(minWidth: 160, maxWidth: 280)
            }
            if controls.options.count > 1 {
                Picker(selection: $controls.sortID) {
                    ForEach(controls.options) { Label($0.title, systemImage: $0.symbol).tag($0.id) }
                } label: {
                    Label(loc("Sort"), systemImage: "arrow.up.arrow.down")
                }
                .pickerStyle(.menu)
                .fixedSize()
                .help(loc("Sort"))
            }
            IconButton(symbol: controls.ascending ? "arrow.up" : "arrow.down",
                       label: controls.ascending ? loc("Ascending order") : loc("Descending order")) {
                controls.ascending.toggle()
            }
        }
    }
}

/// Standard list toolbar: search, sort and scan again. Each view's own filters go before it.
/// Its items wrap onto new lines when they don't fit (see `FlowLayout`).
struct ListToolbar<Item>: View {
    let controls: ListControls<Item>
    var refreshLabel = loc("Scan again")
    /// The refresh is running: its button shows a spinner.
    var busy = false
    /// Another operation (a cleanup) is running: refreshing waits for it.
    var disabled = false
    /// Checks or unchecks everything the page shows.
    var selectAll: BulkSelection? = nil
    var refresh: (() -> Void)? = nil

    var body: some View {
        Group {
            if let selectAll {
                SelectAllToggle(selection: selectAll)
            }
            ListControlsBar(controls: controls)
            if let refresh {
                IconButton(symbol: "arrow.clockwise", label: refreshLabel, disabled: disabled, busy: busy, action: refresh)
            }
        }
    }
}
