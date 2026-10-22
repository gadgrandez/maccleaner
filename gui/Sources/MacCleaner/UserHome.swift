import Foundation

/// The home folder the Rust core analyses: `HOME`, as the core reads it, falling back to the
/// account's home. Using the same source keeps paths shown by the GUI consistent with the core.
let userHome: String = ProcessInfo.processInfo.environment["HOME"].flatMap { $0.isEmpty ? nil : $0 }
    ?? FileManager.default.homeDirectoryForCurrentUser.path

/// `path` with the home folder shown as `~`.
func abbreviatingHome(_ path: String) -> String {
    path == userHome || path.hasPrefix(userHome + "/") ? "~" + path.dropFirst(userHome.count) : path
}
