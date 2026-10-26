import Foundation
import SwiftUI

@main
enum Entry {
    static func main() {
        // Ephemeral mode used by the monitor: posts a notification and exits without opening windows.
        let args = CommandLine.arguments
        if let i = args.firstIndex(of: "--notify") {
            let posted = MonitorAlert(arguments: Array(args[(i + 1)...])).map(Notifier.post) ?? false
            exit(posted ? 0 : 1)
        }
        // The same executable runs the menu bar helper from its own bundle (a login item).
        if Bundle.main.bundleIdentifier == MenuBarHelper.bundleIdentifier {
            MenuBarApp.main()
        } else {
            MacCleanerApp.main()
        }
    }
}

struct MacCleanerApp: App {
    static let mainWindow = "main"

    // No @State: in the macOS 27 SDK it is a macro that Command Line Tools doesn't ship.
    private let state = AppState()

    var body: some Scene {
        // A single window: reopening it from the menu bar brings it back instead of adding another.
        Window("MacCleaner", id: Self.mainWindow) {
            ContentView()
                .environment(state)
                .task { DebugSnapshot.install(state) }
                .frame(minWidth: 980, minHeight: 640)
        }
        .windowResizability(.contentMinSize)
        .windowToolbarStyle(.unified(showsTitle: false))

        Settings {
            SettingsView(preferences: state.preferences)
        }

    }
}
