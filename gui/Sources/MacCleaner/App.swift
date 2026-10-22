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
        MacCleanerApp.main()
    }
}

struct MacCleanerApp: App {
    // No @State: in the macOS 27 SDK it is a macro that Command Line Tools doesn't ship.
    private let state = AppState()

    var body: some Scene {
        WindowGroup("MacCleaner") {
            ContentView()
                .environment(state)
                .task { DebugSnapshot.install(state) }
                .frame(minWidth: 980, minHeight: 640)
        }
        .windowResizability(.contentMinSize)
        .windowToolbarStyle(.unified(showsTitle: false))
    }
}
