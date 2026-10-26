import Foundation
import Observation
import SwiftUI

/// User preferences: kept in `UserDefaults` and passed on to the core, which applies them.
@MainActor
@Observable
final class Preferences {
    private static let deletePermanentlyKey = "deletePermanently"
    private static let showInMenuBarKey = "showInMenuBar"

    /// Cleanups delete for good instead of moving to the Trash. Off by default.
    var deletePermanently: Bool {
        didSet {
            UserDefaults.standard.set(deletePermanently, forKey: Self.deletePermanentlyKey)
            Core.setDeletePermanently(deletePermanently)
        }
    }

    /// Resource monitor in the menu bar. On by default.
    var showInMenuBar: Bool {
        didSet { UserDefaults.standard.set(showInMenuBar, forKey: Self.showInMenuBarKey) }
    }

    init() {
        showInMenuBar = UserDefaults.standard.object(forKey: Self.showInMenuBarKey) as? Bool ?? true
        deletePermanently = UserDefaults.standard.bool(forKey: Self.deletePermanentlyKey)
        Core.setDeletePermanently(deletePermanently)
    }

    // MARK: Wording of the cleanups, which depends on how they remove items

    var removeTitle: String { deletePermanently ? loc("Delete Permanently") : loc("Move to Trash") }
    var removingTitle: String { deletePermanently ? loc("Deleting…") : loc("Moving to the Trash…") }
    var recoveryNote: String {
        deletePermanently
            ? loc("The items will be deleted permanently and can't be recovered.")
            : loc("You can recover the items from the Trash until you empty it.")
    }
}

/// Settings window (MacCleaner ▸ Settings…, ⌘,).
struct SettingsView: View {
    let preferences: Preferences
    private let confirming = State(initialValue: false)

    var body: some View {
        Form {
            Section(loc("Menu bar")) {
                Toggle(isOn: Binding(get: { preferences.showInMenuBar }, set: { preferences.showInMenuBar = $0 })) {
                    Text(loc("Show the resource monitor in the menu bar"))
                    Text(loc("CPU, memory, disk and swap at a glance while MacCleaner is open."))
                }
                .toggleStyle(.switch)
            }
            Section(loc("Cleanup")) {
                Toggle(isOn: Binding(
                    get: { preferences.deletePermanently },
                    // Turning it on is confirmed: from then on nothing can be put back.
                    set: { on in if on { confirming.wrappedValue = true } else { preferences.deletePermanently = false } }
                )) {
                    Text(loc("Delete permanently instead of moving to the Trash"))
                    Text(loc("Frees the space right away, but the items can't be recovered. Items already in the Trash are always deleted permanently."))
                }
                .toggleStyle(.switch)
                if preferences.deletePermanently {
                    Label(loc("Cleanups can't be undone."), systemImage: "exclamationmark.triangle.fill")
                        .font(.callout)
                        .foregroundStyle(.orange)
                }
            }
        }
        .formStyle(.grouped)
        .frame(width: 480)
        .fixedSize(horizontal: false, vertical: true)
        .confirmationDialog(loc("Delete permanently from now on?"), isPresented: confirming.projectedValue) {
            Button(loc("Delete Permanently"), role: .destructive) { preferences.deletePermanently = true }
        } message: {
            Text(loc("Cleaned items won't go to the Trash, so you won't be able to put them back."))
        }
    }
}
