import SwiftUI

// Visual system. Content sits on solid surfaces with a thin border; Liquid Glass (macOS 26+)
// only on the floating control layer, as Apple's guidelines recommend.

// MARK: Tokens

enum Space {
    static let xs: CGFloat = 4
    static let s: CGFloat = 8
    static let m: CGFloat = 12
    static let l: CGFloat = 16
    static let xl: CGFloat = 24
    static let xxl: CGFloat = 32
    /// Side margin of each page's content.
    static let page: CGFloat = 28
}

enum Radius {
    static let control: CGFloat = 8
    static let row: CGFloat = 10
    static let card: CGFloat = 16
    static let hero: CGFloat = 24
}

enum Motion {
    static let hover = Animation.easeOut(duration: 0.2)
    static let standard = Animation.spring(response: 0.35, dampingFraction: 0.86)
    static let stagger = 0.04

    /// Staggered entrance for lists and grids (no motion with *Reduce Motion*).
    static func appear(_ index: Int, reduceMotion: Bool) -> AnyTransition {
        if reduceMotion { return .opacity }
        let delay = Double(min(index, 12)) * stagger
        return .asymmetric(
            insertion: .opacity.combined(with: .offset(y: 10)).animation(standard.delay(delay)),
            removal: .opacity.animation(.easeIn(duration: 0.12))
        )
    }
}

struct ModuleTheme {
    let colors: [Color]

    var gradient: LinearGradient {
        LinearGradient(colors: colors, startPoint: .topLeading, endPoint: .bottomTrailing)
    }

    var tint: Color { colors.first ?? .accentColor }
}

// MARK: Surfaces

extension View {
    /// Content card: solid surface, separator border and a very soft shadow.
    func surfaceCard(cornerRadius: CGFloat = Radius.card) -> some View {
        let shape = RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
        return background(Color(nsColor: .controlBackgroundColor).opacity(0.72), in: shape)
            .overlay(shape.strokeBorder(Color(nsColor: .separatorColor).opacity(0.6), lineWidth: 0.5))
            .shadow(color: .black.opacity(0.05), radius: 6, y: 2)
    }

    /// Floating control layer (action bar, capsules over the content).
    @ViewBuilder
    func floatingGlass(cornerRadius: CGFloat = Radius.hero) -> some View {
        if #available(macOS 26, *) {
            glassEffect(.regular, in: RoundedRectangle(cornerRadius: cornerRadius, style: .continuous))
        } else {
            background(.regularMaterial, in: RoundedRectangle(cornerRadius: cornerRadius, style: .continuous))
                .overlay(RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                    .strokeBorder(Color(nsColor: .separatorColor).opacity(0.5), lineWidth: 0.5))
                .shadow(color: .black.opacity(0.12), radius: 16, y: 6)
        }
    }

    @ViewBuilder
    func primaryActionStyle() -> some View {
        if #available(macOS 26, *) {
            buttonStyle(.glassProminent)
        } else {
            buttonStyle(.borderedProminent)
        }
    }

    @ViewBuilder
    func secondaryActionStyle() -> some View {
        if #available(macOS 26, *) {
            buttonStyle(.glass)
        } else {
            buttonStyle(.bordered)
        }
    }

    /// Hover highlight and link cursor on clickable elements.
    func hoverHighlight(cornerRadius: CGFloat = Radius.row, selected: Bool = false) -> some View {
        modifier(HoverHighlight(cornerRadius: cornerRadius, selected: selected))
    }

    @ViewBuilder
    func clickablePointer() -> some View {
        if #available(macOS 15, *) {
            pointerStyle(.link)
        } else {
            self
        }
    }
}

struct HoverHighlight: ViewModifier {
    let cornerRadius: CGFloat
    let selected: Bool
    // `State` as a plain property: the `@State` macro isn't available in Command Line Tools.
    private let hovering = State(initialValue: false)

    func body(content: Content) -> some View {
        content
            .background {
                RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                    .fill(selected ? Color.accentColor.opacity(0.18) : Color.primary.opacity(hovering.wrappedValue ? 0.06 : 0))
            }
            .contentShape(RoundedRectangle(cornerRadius: cornerRadius, style: .continuous))
            .onHover { inside in withAnimation(Motion.hover) { hovering.wrappedValue = inside } }
            .clickablePointer()
    }
}

// MARK: Layout

/// Lays out items in a row and wraps to the next line when they don't fit, instead of
/// forcing a minimum width that overflows the window.
struct FlowLayout: Layout {
    var spacing: CGFloat = Space.s
    var lineSpacing: CGFloat = Space.s

    private func lines(for subviews: Subviews, maxWidth: CGFloat) -> [[(index: Int, size: CGSize)]] {
        var lines: [[(index: Int, size: CGSize)]] = [[]]
        var x: CGFloat = 0
        for (i, view) in subviews.enumerated() {
            let ideal = view.sizeThatFits(.unspecified)
            let size = ideal.width > maxWidth ? view.sizeThatFits(ProposedViewSize(width: maxWidth, height: nil)) : ideal
            if x > 0 && x + size.width > maxWidth {
                lines.append([])
                x = 0
            }
            lines[lines.count - 1].append((i, size))
            x += size.width + spacing
        }
        return lines
    }

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let maxWidth = proposal.width ?? .infinity
        let rows = lines(for: subviews, maxWidth: maxWidth)
        let height = rows.map { $0.map(\.size.height).max() ?? 0 }.reduce(0, +) + lineSpacing * CGFloat(max(rows.count - 1, 0))
        let widest = rows.map { $0.map(\.size.width).reduce(0, +) + spacing * CGFloat(max($0.count - 1, 0)) }.max() ?? 0
        return CGSize(width: proposal.width ?? widest, height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var y = bounds.minY
        for row in lines(for: subviews, maxWidth: bounds.width) {
            let lineHeight = row.map(\.size.height).max() ?? 0
            var x = bounds.minX
            for item in row {
                // Vertically centered within the line.
                subviews[item.index].place(at: CGPoint(x: x, y: y + (lineHeight - item.size.height) / 2),
                                           proposal: ProposedViewSize(item.size))
                x += item.size.width + spacing
            }
            y += lineHeight + lineSpacing
        }
    }
}

// MARK: Components

struct ModuleIcon: View {
    let symbol: String
    let theme: ModuleTheme
    var size: CGFloat = 44

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: size * 0.28, style: .continuous)
        shape
            .fill(theme.gradient)
            .overlay(shape.fill(LinearGradient(colors: [.white.opacity(0.28), .clear], startPoint: .top, endPoint: .center)))
            .overlay(shape.strokeBorder(.white.opacity(0.18), lineWidth: 0.5))
            .frame(width: size, height: size)
            .overlay {
                Image(systemName: symbol)
                    .font(.system(size: size * 0.46, weight: .semibold))
                    .foregroundStyle(.white)
                    .shadow(color: .black.opacity(0.15), radius: 1, y: 1)
            }
            .shadow(color: theme.tint.opacity(size > 30 ? 0.3 : 0), radius: size * 0.16, y: size * 0.08)
            .accessibilityHidden(true)
    }
}

/// Common layout of every module: tinted background, header and, below it, a bar with the
/// secondary actions (search, sort, filters…). Keeping them apart stops them competing with the
/// title for width.
struct ModulePage<Content: View, Actions: View>: View {
    let destination: Destination
    let subtitle: String
    @ViewBuilder let actions: () -> Actions
    @ViewBuilder let content: () -> Content

    init(_ destination: Destination, subtitle: String,
         @ViewBuilder actions: @escaping () -> Actions = { EmptyView() },
         @ViewBuilder content: @escaping () -> Content) {
        self.destination = destination
        self.subtitle = subtitle
        self.actions = actions
        self.content = content
    }

    private var hasActions: Bool { Actions.self != EmptyView.self }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .center, spacing: Space.l) {
                ModuleIcon(symbol: destination.symbol, theme: destination.theme, size: 48)
                VStack(alignment: .leading, spacing: 2) {
                    Text(destination.title)
                        .font(.system(size: 26, weight: .bold, design: .rounded))
                        .lineLimit(1)
                        .accessibilityAddTraits(.isHeader)
                    Text(subtitle).font(.callout).foregroundStyle(.secondary).lineLimit(2)
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, Space.page)
            .padding(.top, Space.xl)
            .padding(.bottom, hasActions ? Space.m : Space.l)
            if hasActions {
                FlowLayout { actions() }
                    .controlSize(.regular)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, Space.page)
                    .padding(.bottom, Space.l)
            }
            content()
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        }
        .background(alignment: .top) {
            ZStack(alignment: .topLeading) {
                destination.theme.gradient.opacity(0.14)
                RadialGradient(colors: [destination.theme.tint.opacity(0.22), .clear],
                               center: .topLeading, startRadius: 0, endRadius: 420)
            }
            .frame(height: 300)
            .mask(LinearGradient(colors: [.black, .clear], startPoint: .top, endPoint: .bottom))
            .ignoresSafeArea()
            .accessibilityHidden(true)
        }
        .navigationTitle(destination.title)
    }
}

/// Initial state of a module: circular scan button with an animated halo.
struct HeroScan: View {
    let theme: ModuleTheme
    let symbol: String
    let title: String
    let message: String
    var buttonTitle = loc("Scan")
    var busy = false
    var busyText = loc("Scanning…")
    /// Queued behind another scan (see `ScanCoordinator`).
    var waiting = false
    let action: () -> Void

    private var isBusy: Bool { busy || waiting }
    private var statusText: String { waiting ? loc("Waiting: another scan is finishing…") : busyText }
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.controlActiveState) private var activeState

    var body: some View {
        VStack(spacing: Space.xl) {
            Spacer(minLength: 0)
            ZStack {
                halo
                Button(action: action) {
                    ZStack {
                        Circle().fill(theme.gradient)
                        Circle().fill(LinearGradient(colors: [.white.opacity(0.3), .clear], startPoint: .top, endPoint: .center))
                        Circle().strokeBorder(.white.opacity(0.35), lineWidth: 1)
                        if isBusy {
                            ProgressView().controlSize(.large).tint(.white)
                        } else {
                            VStack(spacing: 6) {
                                Image(systemName: symbol).font(.system(size: 38, weight: .semibold))
                                Text(buttonTitle).font(.system(.headline, design: .rounded))
                            }
                            .foregroundStyle(.white)
                        }
                    }
                    .frame(width: 160, height: 160)
                    .shadow(color: theme.tint.opacity(0.45), radius: 26, y: 12)
                    .contentShape(Circle())
                }
                .buttonStyle(PressScaleStyle())
                .clickablePointer()
                .disabled(isBusy)
                .keyboardShortcut(.defaultAction)
                .accessibilityLabel(isBusy ? statusText : buttonTitle)
                .accessibilityHint(message)
            }
            .frame(width: 260, height: 260)
            VStack(spacing: Space.s) {
                Text(isBusy ? statusText : title).font(.system(.title2, design: .rounded).weight(.semibold))
                Text(message).foregroundStyle(.secondary).multilineTextAlignment(.center).frame(maxWidth: 440)
            }
            Spacer(minLength: 0)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(Space.xxl)
    }

    /// Concentric rings that "breathe". They stop while the window is in the background
    /// and with Reduce Motion, so they don't compete with other apps for the GPU.
    private var halo: some View {
        PulseRings(color: NSColor(theme.tint), period: isBusy ? 1.4 : 3.2,
                   running: !reduceMotion && activeState != .inactive)
            .frame(width: 260, height: 260)
            .allowsHitTesting(false)
            .accessibilityHidden(true)
    }
}

/// Rings that expand and fade out. Core Animation runs the animation in the render server
/// (GPU): the app doesn't recompute the view on every frame, unlike `TimelineView`.
struct PulseRings: NSViewRepresentable {
    let color: NSColor
    let period: Double
    let running: Bool

    func makeNSView(context: Context) -> PulseRingsView { PulseRingsView() }

    func updateNSView(_ view: PulseRingsView, context: Context) {
        view.configure(color: color, period: period, running: running)
    }
}

final class PulseRingsView: NSView {
    private let rings: [CAShapeLayer] = (0..<3).map { _ in CAShapeLayer() }
    private var period = 0.0
    private var running = false
    /// Button diameter relative to the canvas: the rings grow from there to the edge.
    private let startRatio = 160.0 / 260.0

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        for ring in rings {
            ring.fillColor = nil
            ring.lineWidth = 1.5
            ring.opacity = 0
            layer?.addSublayer(ring)
        }
    }

    required init?(coder: NSCoder) { nil }

    /// Circle centered on the canvas with the given diameter (fraction of the side).
    /// The path is animated, not the layer's scale: in AppKit the layers' anchor may be
    /// at the corner and scaling would move the rings off center.
    private func circle(_ ratio: Double) -> CGPath {
        let side = min(bounds.width, bounds.height) * ratio
        let rect = CGRect(x: bounds.midX - side / 2, y: bounds.midY - side / 2, width: side, height: side)
        return CGPath(ellipseIn: rect, transform: nil)
    }

    override func layout() {
        super.layout()
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        for ring in rings { ring.frame = bounds }
        CATransaction.commit()
        running ? start() : stop()
    }

    func configure(color: NSColor, period: Double, running: Bool) {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        for ring in rings { ring.strokeColor = color.cgColor }
        CATransaction.commit()
        guard running != self.running || period != self.period else { return }
        self.running = running
        self.period = period
        running ? start() : stop()
    }

    private func start() {
        guard bounds.width > 0 else { return }
        let now = CACurrentMediaTime()
        let from = circle(startRatio), to = circle(0.99)
        for (i, ring) in rings.enumerated() {
            let grow = CABasicAnimation(keyPath: "path")
            grow.fromValue = from
            grow.toValue = to
            let fade = CABasicAnimation(keyPath: "opacity")
            fade.fromValue = 0.35
            fade.toValue = 0
            let group = CAAnimationGroup()
            group.animations = [grow, fade]
            group.duration = period
            group.repeatCount = .infinity
            group.timingFunction = CAMediaTimingFunction(name: .easeOut)
            // Staggered phase from the first frame.
            group.beginTime = now - period * Double(i) / Double(rings.count)
            ring.removeAllAnimations()
            ring.path = from
            ring.opacity = 0
            ring.add(group, forKey: "pulse")
        }
    }

    /// When paused a single static ring remains: no new frames.
    private func stop() {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        for (i, ring) in rings.enumerated() {
            ring.removeAllAnimations()
            ring.path = circle(startRatio * 1.15)
            ring.opacity = i == 0 ? 0.2 : 0
        }
        CATransaction.commit()
    }
}

/// Subtle scale on press, without shifting the layout.
struct PressScaleStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .scaleEffect(configuration.isPressed ? 0.96 : 1)
            .animation(.spring(response: 0.25, dampingFraction: 0.7), value: configuration.isPressed)
    }
}

/// Floating bottom bar: selection summary and the page's main action.
struct ActionBar<Leading: View, Trailing: View>: View {
    @ViewBuilder let leading: () -> Leading
    @ViewBuilder let trailing: () -> Trailing

    var body: some View {
        HStack(spacing: Space.m) {
            leading()
            Spacer(minLength: Space.s)
            trailing()
        }
        .padding(.horizontal, Space.l + 2)
        .padding(.vertical, Space.m)
        .floatingGlass(cornerRadius: 22)
        .padding(.horizontal, Space.xl)
        .padding(.bottom, Space.l)
        .transition(.move(edge: .bottom).combined(with: .opacity))
    }
}

/// "Last clean" line of an action bar. When something failed, its count opens the list of
/// what couldn't be cleaned and why.
struct LastCleanSummary: View {
    let report: CleanReport
    let cleanedBytes: UInt64
    private let showingFailures = State(initialValue: false)

    var body: some View {
        HStack(spacing: Space.xs) {
            Text(loc("Last clean: \(formatBytes(cleanedBytes))"))
            if report.failed > 0 {
                Text("·")
                Button { showingFailures.wrappedValue = true } label: {
                    Label(countFailures(report.failed), systemImage: "exclamationmark.triangle.fill").underline()
                }
                .buttonStyle(.plain)
                .clickablePointer()
                .help(loc("Show what couldn't be cleaned"))
                .popover(isPresented: showingFailures.projectedValue, arrowEdge: .top) {
                    FailureList(outcomes: report.outcomes.filter { !$0.ok })
                }
            }
        }
        .font(.caption)
        .foregroundStyle(report.failed > 0 ? .orange : .secondary)
    }
}

/// Items a cleanup couldn't remove, each with the reason.
struct FailureList: View {
    let outcomes: [CleanOutcome]

    var body: some View {
        VStack(alignment: .leading, spacing: Space.m) {
            Text(loc("Couldn't clean")).font(.headline)
            ScrollView {
                VStack(alignment: .leading, spacing: Space.m) {
                    ForEach(outcomes) { outcome in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(abbreviatingHome(outcome.path)).font(.callout).lineLimit(2).truncationMode(.middle)
                            Text(locCore(outcome.error ?? loc("Unknown error"))).font(.caption).foregroundStyle(.secondary)
                        }
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
            }
            .frame(maxHeight: 280)
        }
        .padding(Space.l)
        .frame(width: 400)
    }
}

struct StatTile: View {
    let symbol: String
    let theme: ModuleTheme
    let value: String
    let label: String
    var detail: String? = nil

    var body: some View {
        HStack(spacing: Space.m + 2) {
            ModuleIcon(symbol: symbol, theme: theme, size: 36)
            VStack(alignment: .leading, spacing: 2) {
                Text(value).font(.system(.title2, design: .rounded).weight(.semibold)).monospacedDigit()
                    .contentTransition(.numericText())
                Text(label).font(.callout).foregroundStyle(.secondary)
                if let detail { Text(detail).font(.caption).foregroundStyle(.secondary) }
            }
            Spacer(minLength: 0)
        }
        .padding(Space.l)
        .frame(maxWidth: .infinity, alignment: .leading)
        .surfaceCard()
        .accessibilityElement(children: .combine)
    }
}

/// Standard selectable row for lists of cleanable items.
struct SelectableRow<Icon: View>: View {
    let isOn: Bool
    let title: String
    var subtitle: String? = nil
    let bytes: UInt64?
    var path: String? = nil
    var disabled = false
    @ViewBuilder let icon: () -> Icon
    let toggle: () -> Void

    var body: some View {
        HStack(spacing: Space.m) {
            Toggle(title, isOn: Binding(get: { isOn }, set: { _ in toggle() }))
                .toggleStyle(.checkbox)
                .labelsHidden()
                .disabled(disabled)
            icon()
            VStack(alignment: .leading, spacing: 2) {
                Text(title).lineLimit(1).truncationMode(.middle).help(title)
                if let subtitle {
                    Text(subtitle).font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle).help(subtitle)
                }
            }
            Spacer(minLength: Space.s)
            if let bytes {
                Text(formatBytes(bytes)).monospacedDigit().foregroundStyle(.secondary)
            }
            if let path {
                RevealButton(path: path)
            }
        }
        .padding(.vertical, 6)
        .padding(.horizontal, Space.s)
        .opacity(disabled ? 0.5 : 1)
        .hoverHighlight()
        .onTapGesture { if !disabled { toggle() } }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(isOn ? [.isButton, .isSelected] : .isButton)
        .accessibilityAction { if !disabled { toggle() } }
    }
}

struct RevealButton: View {
    let path: String

    var body: some View {
        Button {
            NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)])
        } label: {
            Image(systemName: "arrow.up.forward.app").frame(width: 24, height: 24).contentShape(Rectangle())
        }
        .buttonStyle(.borderless)
        .foregroundStyle(.secondary)
        .help(loc("Show in Finder"))
        .accessibilityLabel(loc("Show in Finder"))
    }
}

struct FileIcon: View {
    let path: String
    var size: CGFloat = 28

    var body: some View {
        Image(nsImage: NSWorkspace.shared.icon(forFile: path))
            .resizable()
            .interpolation(.high)
            .frame(width: size, height: size)
            .accessibilityHidden(true)
    }
}

/// Titled block that groups content within a page.
struct CardSection<Content: View>: View {
    let title: String
    var trailing: String? = nil
    @ViewBuilder let content: () -> Content

    var body: some View {
        VStack(alignment: .leading, spacing: Space.s + 2) {
            HStack {
                Text(title).font(.headline).accessibilityAddTraits(.isHeader)
                Spacer()
                if let trailing { Text(trailing).font(.callout).foregroundStyle(.secondary).monospacedDigit() }
            }
            content()
        }
        .padding(Space.l + 2)
        .frame(maxWidth: .infinity, alignment: .leading)
        .surfaceCard()
    }
}

struct Pill: View {
    let text: String
    var color: Color = .secondary

    var body: some View {
        Text(text)
            .font(.caption2.weight(.semibold))
            .padding(.horizontal, 7)
            .padding(.vertical, 2)
            .background(color.opacity(0.16), in: Capsule())
            .foregroundStyle(color)
    }
}

/// Icon button with an accessible name and a comfortable click area.
struct IconButton: View {
    let symbol: String
    let label: String
    var disabled = false
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: symbol).frame(width: 18, height: 18)
        }
        .secondaryActionStyle()
        .disabled(disabled)
        .help(label)
        .accessibilityLabel(label)
    }
}

struct EmptyResult: View {
    let symbol: String
    let title: String
    let message: String

    var body: some View {
        VStack(spacing: Space.m) {
            Image(systemName: symbol)
                .font(.system(size: 48, weight: .semibold))
                .foregroundStyle(LinearGradient(colors: [.green, .teal], startPoint: .top, endPoint: .bottom))
                .symbolEffect(.bounce, value: title)
                .accessibilityHidden(true)
            Text(title).font(.system(.title2, design: .rounded).weight(.semibold))
            Text(message).foregroundStyle(.secondary).multilineTextAlignment(.center).frame(maxWidth: 440)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(Space.xxl)
    }
}

func formatDate(_ seconds: UInt64) -> String {
    guard seconds > 0 else { return "—" }
    return Date(timeIntervalSince1970: TimeInterval(seconds)).formatted(.relative(presentation: .named))
}
