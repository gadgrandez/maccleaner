import Foundation

// Mirror of the types serialized by the Rust core (snake_case keys).

enum CleanAction: String, Codable, Sendable {
    case trash
    case delete
}

struct ScanItem: Codable, Identifiable, Hashable, Sendable {
    let path: String
    let bytes: UInt64
    let files: UInt64
    var id: String { path }
    var name: String { (path as NSString).lastPathComponent }
}

struct CategoryReport: Codable, Identifiable, Sendable {
    let id: String
    let name: String
    let description: String
    let action: CleanAction
    let defaultSelected: Bool
    let bytes: UInt64
    let files: UInt64
    let errors: UInt64
    let items: [ScanItem]
}

struct ScanReport: Codable, Sendable {
    let categories: [CategoryReport]
    let totalBytes: UInt64
    let elapsedMs: UInt64
}

struct CleanOutcome: Codable, Identifiable, Sendable {
    let path: String
    let category: String?
    let action: CleanAction?
    let bytes: UInt64
    let ok: Bool
    let error: String?
    var id: String { path }
}

struct CleanReport: Codable, Sendable {
    let dryRun: Bool
    let trashedBytes: UInt64
    let deletedBytes: UInt64
    let failed: UInt64
    let outcomes: [CleanOutcome]
}

enum Pressure: String, Codable, Sendable {
    case normal, warning, critical, unknown
}

struct SystemStatus: Codable, Sendable {
    let timestamp: UInt64
    let diskTotal: UInt64
    let diskAvailable: UInt64
    let memTotal: UInt64
    let memUsed: UInt64
    let memApp: UInt64
    let memWired: UInt64
    let memCompressed: UInt64
    let swapUsed: UInt64
    let swapTotal: UInt64
    let pressure: Pressure
    let load: [Double]
    let cpuCount: UInt32
}

struct MonitorStatus: Codable, Sendable {
    let installed: Bool
    let running: Bool
    let pid: Int32?
    let program: String?
}

struct MonitorState: Codable, Sendable {
    let startedAt: UInt64
    let lastCheck: UInt64
    let lastJunkScan: UInt64
    let lastJunkBytes: UInt64
}

struct MonitorConfig: Codable, Sendable {
    let checkIntervalSecs: UInt64
    let diskFreeMinPercent: Double
    let junkScanIntervalHours: UInt64
    let junkAlertBytes: UInt64
    let swapAlertBytes: UInt64
}

struct MonitorInfo: Codable, Sendable {
    let state: MonitorState?
    let config: MonitorConfig
}

struct AccessStatus: Codable, Sendable {
    let fullDiskAccess: Bool
}

func formatBytes(_ bytes: UInt64) -> String {
    ByteCountFormatter.string(fromByteCount: Int64(clamping: bytes), countStyle: .file)
}

// MARK: Applications

enum AppSource: String, Codable, Sendable {
    case appStore = "app_store"
    case setapp
    case other
}

struct AppInfo: Codable, Identifiable, Hashable, Sendable {
    let name: String
    let bundleId: String?
    let version: String?
    let path: String
    let bytes: UInt64
    let isApple: Bool
    let source: AppSource
    let sparkleFeedUrl: String?
    let archs: [String]
    var id: String { path }
    var isUniversal: Bool { archs.count > 1 }
}

struct LeftoverItem: Codable, Identifiable, Hashable, Sendable {
    let path: String
    let kind: String
    let bytes: UInt64
    var id: String { path }
    var name: String { (path as NSString).lastPathComponent }

    var kindTitle: String {
        switch kind {
        case "cache": loc("Cache")
        case "preferences": loc("Preferences")
        case "app_support": loc("App data")
        case "container": loc("Container")
        case "group_container": loc("Shared container")
        case "saved_state": loc("Saved state")
        case "logs": loc("Logs")
        case "launch_agent": loc("Launch agent")
        case "cookies": loc("Cookies")
        case "webkit", "http_storage": loc("Web data")
        default: loc("Other")
        }
    }
}

struct AppLeftovers: Codable, Sendable {
    let app: AppInfo
    let items: [LeftoverItem]
    let totalBytes: UInt64
}

struct OrphanGroup: Codable, Identifiable, Sendable {
    let id: String
    let items: [LeftoverItem]
    let totalBytes: UInt64
}

struct ExtensionItem: Codable, Identifiable, Hashable, Sendable {
    let name: String
    let path: String
    let kind: String
    let scope: String
    let bytes: UInt64
    let removable: Bool
    var id: String { path }

    var kindTitle: String {
        switch kind {
        case "preference_pane": loc("Preference panes")
        case "quicklook": loc("Quick Look plugins")
        case "spotlight": loc("Spotlight importers")
        case "internet_plugin": loc("Internet plugins")
        case "screen_saver": loc("Screen savers")
        case "services": loc("Services")
        case "mail_bundle": loc("Mail plugins")
        default: kind
        }
    }
}

struct UninstallRequest: Codable, Sendable {
    let appPath: String?
    let leftoverPaths: [String]
    let dryRun: Bool
}

struct PathsRequest: Codable, Sendable {
    let paths: [String]
    let dryRun: Bool
}

// MARK: Performance

struct LaunchItem: Codable, Identifiable, Hashable, Sendable {
    let label: String
    let plistPath: String
    let program: String?
    let scope: String
    let runAtLoad: Bool
    let keepAlive: Bool
    let enabled: Bool
    let loaded: Bool
    let pid: Int32?
    let appName: String?
    let removable: Bool
    var id: String { plistPath }

    var scopeTitle: String {
        switch scope {
        case "user_agent": loc("User agents")
        case "global_agent": loc("Global agents")
        default: loc("System daemons")
        }
    }
}

struct MaintenanceTask: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let name: String
    let description: String
    let requiresAdmin: Bool
    let icon: String
}

struct TaskResult: Codable, Identifiable, Sendable {
    let id: String
    let ok: Bool
    let output: String
    let error: String?
}

struct ProcessEntry: Codable, Identifiable, Sendable {
    let pid: Int32
    let name: String
    let path: String?
    let memory: UInt64
    let cpuTimeNs: UInt64
    let userOwned: Bool
    var id: Int32 { pid }
}

// MARK: Files

struct FileInfo: Codable, Identifiable, Hashable, Sendable {
    let path: String
    let bytes: UInt64
    let modified: UInt64
    let accessed: UInt64
    let kind: String
    var id: String { path }
    var name: String { (path as NSString).lastPathComponent }

    var kindTitle: String {
        switch kind {
        case "video": loc("Videos")
        case "image": loc("Images")
        case "audio": loc("Audio")
        case "archive": loc("Archives")
        case "disk_image": loc("Disk images")
        case "document": loc("Documents")
        case "app": loc("Apps")
        case "code": loc("Code")
        default: loc("Other")
        }
    }
}

struct LargeFilesRequest: Codable, Sendable {
    var roots: [String]? = nil
    var minBytes: UInt64
    var olderThanDays: UInt32?
    var limit: UInt32 = 500
    var includeLibrary = false
}

struct LargeFilesReport: Codable, Sendable {
    let files: [FileInfo]
    let scannedFiles: UInt64
    let elapsedMs: UInt64
}

struct DuplicatesRequest: Codable, Sendable {
    var roots: [String]? = nil
    var minBytes: UInt64 = 1_000_000
    var limit: UInt32 = 300
}

struct DupGroup: Codable, Identifiable, Sendable {
    let bytes: UInt64
    let paths: [String]
    let wastedBytes: UInt64
    var id: String { paths.first ?? "" }
}

struct DuplicatesReport: Codable, Sendable {
    let groups: [DupGroup]
    let totalWasted: UInt64
    let scannedFiles: UInt64
    let elapsedMs: UInt64
}

struct SpaceNode: Codable, Identifiable, Hashable, Sendable {
    let name: String
    let path: String
    let bytes: UInt64
    let files: UInt64
    let isDir: Bool
    let children: [SpaceNode]
    let othersBytes: UInt64
    var id: String { path }
}

// MARK: Privacy

struct PrivacyItem: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let name: String
    let paths: [String]
    let bytes: UInt64
    let accessible: Bool
}

struct PrivacySource: Codable, Identifiable, Sendable {
    let id: String
    let name: String
    let running: Bool
    let items: [PrivacyItem]
}

struct PrivacyReport: Codable, Sendable {
    let sources: [PrivacySource]
}

struct PrivacyTarget: Codable, Hashable, Sendable {
    let source: String
    let item: String
}

struct PrivacyCleanRequest: Codable, Sendable {
    let items: [PrivacyTarget]
    let dryRun: Bool
}

// MARK: Development

struct DevArtifact: Codable, Identifiable, Hashable, Sendable {
    let path: String
    let kind: String
    let ecosystem: String
    let bytes: UInt64
    var id: String { path }
}

struct DevProject: Codable, Identifiable, Sendable {
    let path: String
    let name: String
    let ecosystems: [String]
    let lastActivity: UInt64
    let inactiveDays: UInt32
    let git: Bool
    let artifacts: [DevArtifact]
    let bytes: UInt64
    var id: String { path }
}

struct DevProjectsReport: Codable, Sendable {
    let projects: [DevProject]
    let totalBytes: UInt64
    let scannedDirs: UInt64
    let elapsedMs: UInt64
}

struct DevProjectsRequest: Codable, Sendable {
    var roots: [String]? = nil
    var includeProtected = false
}

enum ToolAction: String, Codable, Sendable {
    case trash, command
}

struct ToolCache: Codable, Identifiable, Hashable, Sendable {
    let id: String
    let name: String
    let tool: String
    let paths: [String]
    let bytes: UInt64
    let defaultSelected: Bool
    let note: String?
    let action: ToolAction
}

struct ToolchainReport: Codable, Sendable {
    let items: [ToolCache]
    let totalBytes: UInt64
    let elapsedMs: UInt64
}

struct CommandResult: Codable, Identifiable, Sendable {
    let id: String
    let ok: Bool
    let output: String
    let error: String?
}
