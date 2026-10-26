import CMacCleaner
import Foundation

// Core operations grouped by module. Every destructive action is sent with
// `dryRun: false`: the user has already confirmed it in the UI.

extension Core {
    // MARK: System and monitor

    static func accessStatus() async throws -> AccessStatus { try await call { mcl_access_status() } }
    static func systemStatus() async throws -> SystemStatus { try await call { mcl_system_status() } }
    static func monitorStatus() async throws -> MonitorStatus { try await call { mcl_monitor_status() } }
    static func monitorInfo() async throws -> MonitorInfo { try await call { mcl_monitor_info() } }
    static func uninstallMonitor() async throws -> MonitorStatus { try await call { mcl_monitor_uninstall() } }

    static func installMonitor(program: String) async throws -> MonitorStatus {
        try await call(argument: program) { mcl_monitor_install($0) }
    }

    // MARK: System junk

    static func scan() async throws -> ScanReport { try await call { mcl_scan() } }

    static func clean(paths: [String]) async throws -> CleanReport {
        try await call(request: PathsRequest(paths: paths, dryRun: false)) { mcl_clean($0) }
    }

    /// Cleanups delete for good instead of moving to the Trash (instant, so called inline).
    static func setDeletePermanently(_ on: Bool) {
        let _: String? = try? decode(mcl_set_delete_permanently(on ? 1 : 0))
    }

    /// Progress of the cleanup in course, whichever module started it.
    static func cleanProgress() async throws -> CleanProgress { try await call { mcl_clean_progress() } }

    // MARK: Developer junk

    static func devProjects(_ request: DevProjectsRequest = DevProjectsRequest()) async throws -> DevProjectsReport {
        try await call(request: request) { mcl_dev_projects($0) }
    }

    static func devToolchains() async throws -> ToolchainReport { try await call { mcl_dev_toolchains() } }

    static func devClean(paths: [String]) async throws -> CleanReport {
        try await call(request: PathsRequest(paths: paths, dryRun: false)) { mcl_dev_clean($0) }
    }

    static func devRun(ids: [String]) async throws -> [CommandResult] {
        try await call(request: ids) { mcl_dev_run($0) }
    }

    // MARK: Privacy

    static func privacyScan() async throws -> PrivacyReport { try await call { mcl_privacy_scan() } }

    static func privacyClean(_ targets: [PrivacyTarget]) async throws -> CleanReport {
        try await call(request: PrivacyCleanRequest(items: targets, dryRun: false)) { mcl_privacy_clean($0) }
    }

    // MARK: Performance

    static func launchItems() async throws -> [LaunchItem] { try await call { mcl_launch_items() } }

    static func setLaunchItem(label: String, enabled: Bool) async throws -> LaunchItem {
        try await call(argument: label) { mcl_launch_item_set_enabled($0, enabled ? 1 : 0) }
    }

    static func removeLaunchItem(plistPath: String) async throws -> CleanReport {
        try await call(argument: plistPath) { mcl_launch_item_remove($0, 0) }
    }

    static func stopLaunchItem(label: String) async throws -> Bool {
        try await call(argument: label) { mcl_launch_item_stop($0) }
    }

    static func maintenanceTasks() async throws -> [MaintenanceTask] { try await call { mcl_maintenance_tasks() } }

    static func runMaintenance(ids: [String]) async throws -> [TaskResult] {
        try await call(request: ids) { mcl_maintenance_run($0) }
    }

    static func topProcesses(limit: UInt32 = 60) async throws -> [ProcessEntry] {
        try await call { mcl_top_processes(limit) }
    }

    // MARK: Applications

    static func apps() async throws -> [AppInfo] { try await call { mcl_apps_list() } }
    static func orphans() async throws -> [OrphanGroup] { try await call { mcl_apps_orphans() } }
    static func extensions() async throws -> [ExtensionItem] { try await call { mcl_extensions_list() } }

    static func leftovers(appPath: String) async throws -> AppLeftovers {
        try await call(argument: appPath) { mcl_apps_leftovers($0) }
    }

    static func uninstall(_ request: UninstallRequest) async throws -> CleanReport {
        try await call(request: request) { mcl_apps_uninstall($0) }
    }

    static func removeExtensions(paths: [String]) async throws -> CleanReport {
        try await call(request: PathsRequest(paths: paths, dryRun: false)) { mcl_extensions_remove($0) }
    }

    // MARK: Files

    static func largeFiles(_ request: LargeFilesRequest) async throws -> LargeFilesReport {
        try await call(request: request) { mcl_large_files($0) }
    }

    static func duplicates(_ request: DuplicatesRequest) async throws -> DuplicatesReport {
        try await call(request: request) { mcl_duplicates($0) }
    }

    static func spaceTree(path: String, depth: UInt32 = 2, top: UInt32 = 14) async throws -> SpaceNode {
        try await call(argument: path) { mcl_space_tree($0, depth, top) }
    }

    static func trash(paths: [String]) async throws -> CleanReport {
        try await call(request: PathsRequest(paths: paths, dryRun: false)) { mcl_trash_paths($0) }
    }
}
