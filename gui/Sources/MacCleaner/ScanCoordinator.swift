import Foundation
import Observation

/// Single disk-scan queue for the whole app.
///
/// Scans run one at a time, in arrival order: several simultaneous walks multiply memory
/// pressure and I/O threads, and hurt the other apps.
@MainActor
@Observable
final class ScanCoordinator {
    /// Scan currently running (display name).
    private(set) var running: String?
    /// Scans waiting for their turn, in order.
    private(set) var waiting: [String] = []
    @ObservationIgnored private var tail: Task<Void, Never>?

    func isWaiting(_ name: String) -> Bool { waiting.contains(name) }

    /// Enqueues `work` behind the pending scans and waits for it to finish.
    func perform(_ name: String, _ work: @escaping @MainActor () async -> Void) async {
        let previous = tail
        waiting.append(name)
        let task = Task { @MainActor in
            await previous?.value
            if let i = self.waiting.firstIndex(of: name) { self.waiting.remove(at: i) }
            self.running = name
            await work()
            self.running = nil
        }
        tail = task
        await task.value
    }
}

/// Entry point for a specific scan (one module). If it is already running or queued, callers
/// wait for that same scan instead of starting another: there are never two identical ones at once.
@MainActor
final class ScanSlot {
    let name: String
    private let coordinator: ScanCoordinator
    private var flight: Task<Void, Never>?

    init(_ name: String, coordinator: ScanCoordinator) {
        self.name = name
        self.coordinator = coordinator
    }

    /// Queued, waiting for another scan.
    var isWaiting: Bool { coordinator.isWaiting(name) }

    func run(_ work: @escaping @MainActor () async -> Void) async {
        if let flight {
            await flight.value
            return
        }
        let task = Task { await coordinator.perform(name, work) }
        flight = task
        await task.value
        flight = nil
    }
}
