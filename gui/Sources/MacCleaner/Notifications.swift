import Foundation
import UserNotifications

/// Alerts sent by the background monitor as `--notify <kind> <values…>`. The monitor only sends
/// raw values; the text is composed here, in the device language.
struct MonitorAlert {
    let title: String
    let body: String

    init?(arguments: [String]) {
        guard let kind = arguments.first else { return nil }
        let values = Array(arguments.dropFirst())
        func value(_ i: Int) -> String { values.indices.contains(i) ? values[i] : "" }
        switch kind {
        case "disk_low":
            let percent = (Double(value(1)) ?? 0) / 100
            title = loc("Low disk space")
            body = loc("\(value(0)) free (\(percent.formatted(.percent.precision(.fractionLength(0))))).")
        case "memory_pressure":
            title = loc("High memory pressure")
            body = loc("The system is running out of memory. Quit apps you're not using.")
        case "swap_high":
            title = loc("High swap usage")
            body = loc("\(value(0)) in swap.")
        case "junk_ready":
            title = loc("Junk ready to clean")
            body = loc("\(value(0)) can be reclaimed. Open MacCleaner to review it.")
        default:
            return nil
        }
    }
}

enum Notifier {
    static func post(_ alert: MonitorAlert) -> Bool {
        // UNUserNotificationCenter requires running inside a bundle.
        guard Bundle.main.bundleIdentifier != nil else { return false }
        let center = UNUserNotificationCenter.current()
        let done = DispatchSemaphore(value: 0)
        let result = Flag()
        center.requestAuthorization(options: [.alert, .sound]) { granted, _ in
            guard granted else {
                done.signal()
                return
            }
            let content = UNMutableNotificationContent()
            content.title = alert.title
            content.body = alert.body
            let request = UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil)
            center.add(request) { error in
                result.value = error == nil
                done.signal()
            }
        }
        _ = done.wait(timeout: .now() + 10)
        return result.value
    }

    private final class Flag: @unchecked Sendable {
        var value = false
    }
}
