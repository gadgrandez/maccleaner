import AppKit
import Foundation
import Observation

@MainActor
@Observable
final class Alerts {
    var message: String?

    /// Runs an operation and turns its error into an alert.
    func run<T>(_ operation: () async throws -> T) async -> T? {
        do {
            return try await operation()
        } catch {
            message = error.localizedDescription
            return nil
        }
    }
}

/// Root state: one store per module, created only once.
@MainActor
@Observable
final class AppState {
    var destination: Destination? = .smartCare
    /// `nil` until the first check.
    var fullDiskAccess: Bool?
    let alerts: Alerts
    let preferences = Preferences()
    let scans = ScanCoordinator()
    let junk: JunkStore
    let dev: DevStore
    let privacy: PrivacyStore
    let status: StatusStore
    let maintenance: MaintenanceStore
    let launch: LaunchStore
    let processes: ProcessStore
    let apps: AppsStore
    let updates: UpdatesStore
    let extensions: ExtensionsStore
    let spaceLens: SpaceLensStore
    let largeFiles: LargeFilesStore
    let duplicates: DuplicatesStore
    let smartCare: SmartCareStore

    init() {
        let alerts = Alerts()
        self.alerts = alerts
        junk = JunkStore(alerts: alerts, scans: scans)
        dev = DevStore(alerts: alerts, scans: scans)
        privacy = PrivacyStore(alerts: alerts, scans: scans)
        status = StatusStore(alerts: alerts)
        maintenance = MaintenanceStore(alerts: alerts)
        launch = LaunchStore(alerts: alerts)
        processes = ProcessStore(alerts: alerts)
        apps = AppsStore(alerts: alerts, scans: scans)
        updates = UpdatesStore(alerts: alerts)
        extensions = ExtensionsStore(alerts: alerts)
        spaceLens = SpaceLensStore(alerts: alerts, scans: scans)
        largeFiles = LargeFilesStore(alerts: alerts, scans: scans)
        duplicates = DuplicatesStore(alerts: alerts, scans: scans)
        smartCare = SmartCareStore()
        smartCare.attach(self)
    }

    func refreshAccess() async {
        let granted = (try? await Core.accessStatus())?.fullDiskAccess
        if granted != fullDiskAccess { fullDiskAccess = granted }
    }

    func openFullDiskAccessSettings() {
        let url = URL(string: "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AllFiles")!
        NSWorkspace.shared.open(url)
    }
}

extension Set {
    /// Inserts the element if absent; removes it if present.
    mutating func toggle(_ element: Element) {
        if contains(element) { remove(element) } else { insert(element) }
    }
}
