import Charts
import SwiftUI

@MainActor
@Observable
final class StatusStore {
    struct Sample: Identifiable {
        let id: Int
        let memory: Double
        let load: Double
    }

    private let alerts: Alerts
    var system: SystemStatus?
    var history: [Sample] = []
    var monitor: MonitorStatus?
    var monitorInfo: MonitorInfo?
    var monitorBusy = false
    private var tick = 0

    init(alerts: Alerts) { self.alerts = alerts }

    var monitorExecutable: String? { Bundle.main.url(forAuxiliaryExecutable: "maccleaner-monitor")?.path }

    func refresh() async {
        guard let s = try? await Core.systemStatus() else { return }
        system = s
        tick += 1
        history.append(Sample(id: tick,
                              memory: Double(s.memUsed) / Double(max(s.memTotal, 1)) * 100,
                              load: (s.load.first ?? 0) / Double(max(s.cpuCount, 1)) * 100))
        if history.count > 90 { history.removeFirst(history.count - 90) }
    }

    func refreshMonitor() async {
        monitor = try? await Core.monitorStatus()
        monitorInfo = try? await Core.monitorInfo()
    }

    func setMonitor(enabled: Bool) async {
        monitorBusy = true
        defer { monitorBusy = false }
        let program = monitorExecutable
        _ = await alerts.run {
            if enabled {
                guard let program else { throw CoreError(message: loc("The monitor is only available when running MacCleaner.app")) }
                return try await Core.installMonitor(program: program)
            }
            return try await Core.uninstallMonitor()
        }
        await refreshMonitor()
    }
}

struct StatusView: View {
    @Environment(AppState.self) private var state

    var body: some View {
        let store = state.status
        ModulePage(.status, subtitle: loc("Disk, memory and load in real time. Only measured while this view is open.")) {
            ScrollView {
                VStack(spacing: 14) {
                    if let s = store.system {
                        gauges(s)
                        historyChart(store.history)
                    } else {
                        ProgressView().frame(maxWidth: .infinity, minHeight: 200)
                    }
                    monitorCard(store)
                }
                .padding(.horizontal, 24)
                .padding(.bottom, 24)
            }
        }
        .task {
            await store.refreshMonitor()
            while !Task.isCancelled {
                await store.refresh()
                try? await Task.sleep(for: .seconds(2))
            }
        }
    }

    private func gauges(_ s: SystemStatus) -> some View {
        let diskUsed = s.diskTotal - min(s.diskAvailable, s.diskTotal)
        let diskFrac = Double(diskUsed) / Double(max(s.diskTotal, 1))
        let memFrac = Double(s.memUsed) / Double(max(s.memTotal, 1))
        let swapFrac = s.swapTotal > 0 ? Double(s.swapUsed) / Double(s.swapTotal) : 0
        let loadFrac = (s.load.first ?? 0) / Double(max(s.cpuCount, 1))
        return HStack(spacing: 14) {
            RingCard(title: loc("Disk"), value: "\(formatBytes(s.diskAvailable))", caption: loc("free of \(formatBytes(s.diskTotal))"),
                     fraction: diskFrac, colors: diskFrac > 0.9 ? [.red, .orange] : [.teal, .blue])
            RingCard(title: loc("Memory"), value: formatBytes(s.memUsed), caption: loc("of \(formatBytes(s.memTotal)) · \(pressureText(s.pressure)) pressure"),
                     fraction: memFrac, colors: pressureColors(s.pressure))
            RingCard(title: loc("Swap"), value: formatBytes(s.swapUsed), caption: loc("of \(formatBytes(s.swapTotal))"),
                     fraction: swapFrac, colors: swapFrac > 0.75 ? [.orange, .red] : [.indigo, .purple])
            RingCard(title: loc("CPU"), value: String(format: "%.2f", s.load.first ?? 0), caption: locCount(s.cpuCount, one: { loc("load average · \($0) core") }, other: { loc("load average · \($0) cores") }),
                     fraction: loadFrac, colors: [.pink, .orange])
        }
    }

    private func historyChart(_ history: [StatusStore.Sample]) -> some View {
        CardSection(title: loc("Last 3 minutes")) {
            Chart(history) { sample in
                LineMark(x: .value("t", sample.id), y: .value("%", sample.memory), series: .value(loc("Series"), loc("Memory")))
                    .foregroundStyle(.teal)
                    .interpolationMethod(.catmullRom)
                LineMark(x: .value("t", sample.id), y: .value("%", sample.load), series: .value(loc("Series"), loc("CPU")))
                    .foregroundStyle(.pink)
                    .interpolationMethod(.catmullRom)
            }
            .chartYScale(domain: 0...100)
            .chartXAxis(.hidden)
            .chartYAxis { AxisMarks(values: [0, 50, 100]) { v in AxisGridLine(); AxisValueLabel { Text(verbatim: "\(v.as(Int.self) ?? 0) %") } } }
            .chartForegroundStyleScale([loc("Memory"): Color.teal, loc("CPU"): Color.pink])
            .frame(height: 160)
        }
    }

    private func monitorCard(_ store: StatusStore) -> some View {
        CardSection(title: loc("Background monitor")) {
            HStack(alignment: .top, spacing: 14) {
                ModuleIcon(symbol: "bell.badge.fill", theme: ModuleTheme(colors: [.orange, .pink]), size: 40)
                VStack(alignment: .leading, spacing: 6) {
                    Text(loc("Alerts you to low disk space, memory pressure, high swap and accumulated junk. A low-priority process of about 2 MB."))
                        .foregroundStyle(.secondary)
                    if let m = store.monitor {
                        Label(m.running ? loc("Active (pid \(m.pid.map(String.init) ?? "?"))") : (m.installed ? loc("Installed, stopped") : loc("Off")),
                              systemImage: m.running ? "checkmark.circle.fill" : "circle")
                            .foregroundStyle(m.running ? .green : .secondary)
                    }
                    if let st = store.monitorInfo?.state, st.lastJunkScan > 0 {
                        Text(loc("Last scan \(formatDate(st.lastJunkScan)): \(formatBytes(st.lastJunkBytes)) reclaimable"))
                            .font(.callout).foregroundStyle(.secondary)
                    }
                    if store.monitorExecutable == nil {
                        Text(loc("Open MacCleaner.app to turn it on.")).font(.caption).foregroundStyle(.orange)
                    }
                }
                Spacer()
                Toggle("", isOn: Binding(get: { store.monitor?.installed ?? false },
                                         set: { v in Task { await store.setMonitor(enabled: v) } }))
                    .toggleStyle(.switch).labelsHidden()
                    .disabled(store.monitorBusy || (store.monitorExecutable == nil && !(store.monitor?.installed ?? false)))
            }
        }
    }

    private func pressureText(_ p: Pressure) -> String {
        switch p {
        case .normal: loc("normal")
        case .warning: loc("elevated")
        case .critical: loc("critical")
        case .unknown: "—"
        }
    }

    private func pressureColors(_ p: Pressure) -> [Color] {
        switch p {
        case .normal, .unknown: [.green, .teal]
        case .warning: [.yellow, .orange]
        case .critical: [.red, .orange]
        }
    }
}

struct RingCard: View {
    let title: String
    let value: String
    let caption: String
    let fraction: Double
    let colors: [Color]

    var body: some View {
        VStack(spacing: 12) {
            ZStack {
                Circle().stroke(.quaternary, lineWidth: 12)
                Circle()
                    .trim(from: 0, to: min(max(fraction, 0.001), 1))
                    .stroke(AngularGradient(colors: colors + [colors[0]], center: .center),
                            style: StrokeStyle(lineWidth: 12, lineCap: .round))
                    .rotationEffect(.degrees(-90))
                    .animation(.smooth, value: fraction)
                Text(verbatim: "\(Int((fraction * 100).rounded())) %").font(.title3.weight(.semibold)).monospacedDigit()
            }
            .frame(width: 104, height: 104)
            VStack(spacing: 2) {
                Text(title).font(.subheadline).foregroundStyle(.secondary)
                Text(value).font(.headline).monospacedDigit()
                Text(caption).font(.caption).foregroundStyle(.tertiary).multilineTextAlignment(.center).lineLimit(2)
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity)
        .surfaceCard()
    }
}
