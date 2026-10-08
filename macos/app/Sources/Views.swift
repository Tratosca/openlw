// OpenLW views: glass panel, meters, input matrix.
// AppKit only (10.13 minimum). Liquid Glass on macOS 26+, NSVisualEffectView below.

import AppKit

enum Theme {
    /// Unpatched crosspoint, legible on light glass.
    static let emptyCell = NSColor.labelColor.withAlphaComponent(0.12)
    static var accent: NSColor {
        if #available(macOS 10.14, *) { return .controlAccentColor }
        return .systemBlue
    }
    static let meterGreen = NSColor(calibratedRed: 0.20, green: 0.78, blue: 0.35, alpha: 1)
    static let meterAmber = NSColor(calibratedRed: 0.98, green: 0.72, blue: 0.15, alpha: 1)
    static let meterRed = NSColor(calibratedRed: 0.95, green: 0.26, blue: 0.21, alpha: 1)
    static let title = NSFont.systemFont(ofSize: 13, weight: .semibold)
    static let body = NSFont.systemFont(ofSize: 12)
    static let mono = NSFont.monospacedDigitSystemFont(ofSize: 12, weight: .regular)
    static let small = NSFont.systemFont(ofSize: 11)
    static let tiny = NSFont.systemFont(ofSize: 9, weight: .semibold)
}

/// Translucent container: Liquid Glass if available, otherwise vibrancy.
final class GlassPanel: NSView {
    let content = NSView()

    init(title: String) {
        super.init(frame: .zero)
        translatesAutoresizingMaskIntoConstraints = false
        content.translatesAutoresizingMaskIntoConstraints = false
        let background: NSView
        if #available(macOS 26.0, *) {
            let glass = NSGlassEffectView()
            glass.cornerRadius = 14
            glass.contentView = NSView()
            background = glass
        } else {
            let effect = NSVisualEffectView()
            effect.blendingMode = .withinWindow
            effect.state = .active
            if #available(macOS 10.14, *) {
                effect.material = .contentBackground
            }
            effect.wantsLayer = true
            effect.layer?.cornerRadius = 10
            effect.layer?.masksToBounds = true
            background = effect
        }
        background.translatesAutoresizingMaskIntoConstraints = false
        addSubview(background)
        let label = NSTextField(labelWithString: title)
        label.font = Theme.title
        label.translatesAutoresizingMaskIntoConstraints = false
        addSubview(label)
        addSubview(content)
        NSLayoutConstraint.activate([
            background.leadingAnchor.constraint(equalTo: leadingAnchor),
            background.trailingAnchor.constraint(equalTo: trailingAnchor),
            background.topAnchor.constraint(equalTo: topAnchor),
            background.bottomAnchor.constraint(equalTo: bottomAnchor),
            label.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 16),
            label.topAnchor.constraint(equalTo: topAnchor, constant: 12),
            content.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 16),
            content.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -16),
            content.topAnchor.constraint(equalTo: label.bottomAnchor, constant: 10),
            content.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -14),
        ])
    }

    required init?(coder: NSCoder) { fatalError("unused") }
}

/// Horizontal meter, one segment per channel; −60…0 dBFS scale.
final class MeterView: NSView {
    var levels: [Double?] = [] { didSet { if levels != oldValue { needsDisplay = true } } }

    // Own layer: level changes redraw only the meter.
    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layerContentsRedrawPolicy = .onSetNeedsDisplay
    }

    required init?(coder: NSCoder) { fatalError("unused") }

    override var intrinsicContentSize: NSSize { NSSize(width: 120, height: 10) }

    override func draw(_ dirtyRect: NSRect) {
        let n = max(levels.count, 1)
        let gap: CGFloat = 2
        let h = (bounds.height - gap * CGFloat(n - 1)) / CGFloat(n)
        for (i, level) in levels.enumerated() {
            let y = CGFloat(i) * (h + gap)
            let track = NSRect(x: 0, y: y, width: bounds.width, height: h)
            NSColor.quaternaryLabelColor.setFill()
            NSBezierPath(roundedRect: track, xRadius: 2, yRadius: 2).fill()
            guard let db = level, db > -60 else { continue }
            let frac = CGFloat(min(1, max(0, (db + 60) / 60)))
            let color = db > -6 ? Theme.meterRed : (db > -18 ? Theme.meterAmber : Theme.meterGreen)
            color.setFill()
            NSBezierPath(roundedRect: NSRect(x: 0, y: y, width: bounds.width * frac, height: h), xRadius: 2, yRadius: 2).fill()
        }
    }
}

/// Flat meter bars (track and level) in their own layer, above a view whose drawing is costly:
/// level changes redraw only the bars. Transparent to clicks.
final class MeterBarsView: NSView {
    struct Bar: Equatable {
        var rect: NSRect
        var level: Double?
    }

    var bars: [Bar] = [] { didSet { if bars != oldValue { needsDisplay = true } } }

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        layerContentsRedrawPolicy = .onSetNeedsDisplay
    }

    required init?(coder: NSCoder) { fatalError("unused") }

    override var isFlipped: Bool { true }

    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func draw(_ dirtyRect: NSRect) {
        for bar in bars {
            NSColor.quaternaryLabelColor.setFill()
            bar.rect.fill()
            if let db = bar.level, db > -60 {
                (db > -6 ? Theme.meterRed : (db > -18 ? Theme.meterAmber : Theme.meterGreen)).setFill()
                NSRect(x: bar.rect.minX, y: bar.rect.minY, width: bar.rect.width * CGFloat((db + 60) / 60),
                       height: bar.rect.height).fill()
            }
        }
    }
}

/// Column-header group: a pair (duplex layout) or a device (multi layout), coupled in stereo
/// or not; title and state (meters: `InputGridView.headerLevels`).
struct GridHeader: Equatable {
    let columns: Range<Int>
    let title: String
    let status: String
    let coupled: Bool
}

/// Display-synchronized callback while started: `CADisplayLink` on macOS 14 and later, 60 Hz
/// timer before. Runs in common modes (menus, resizing).
final class FrameTicker: NSObject {
    private let action: () -> Void
    private var link: AnyObject?
    private var timer: Timer?

    init(_ action: @escaping () -> Void) {
        self.action = action
    }

    var running: Bool { link != nil || timer != nil }

    func start(for view: NSView) {
        guard !running else { return }
        if #available(macOS 14.0, *) {
            let l = view.displayLink(target: self, selector: #selector(fire))
            l.preferredFrameRateRange = CAFrameRateRange(minimum: 30, maximum: 60, preferred: 60)
            l.add(to: .main, forMode: .common)
            link = l
        } else {
            let t = Timer(timeInterval: 1.0 / 60, repeats: true) { [weak self] _ in self?.action() }
            RunLoop.main.add(t, forMode: .common)
            timer = t
        }
    }

    func stop() {
        if #available(macOS 14.0, *) {
            (link as? CADisplayLink)?.invalidate()
        }
        link = nil
        timer?.invalidate()
        timer = nil
    }

    @objc private func fire() { action() }
}

/// Sides of a stereo source feeding an uncoupled column; `tag`: other source channel
/// (surround), shown as is.
struct GridSides: Equatable {
    var left = false
    var right = false
    var tag: String?
}

/// Input matrix row: source (discovered/manual) and its crosspoints.
struct GridRow {
    let source: DiscoveredSource
    /// Coupled groups fed by this source (header index → label; empty: plain stereo).
    let groups: [Int: String]
    /// Uncoupled columns fed by this source.
    let sides: [Int: GridSides]
    /// Transmitting terminal, or row provenance if source not advertised.
    let origin: String
    /// Removable row (manual channel or configured unadvertised stream).
    let removable: Bool
}

/// One part of the input patch matrix (see `InputMatrixView`): source labels, or the cells,
/// whose columns are device channels (duplex layout) or input devices (multi layout).
final class InputGridView: NSView {
    enum Part { case labels, cells }
    let part: Part

    init(part: Part) {
        self.part = part
        super.init(frame: .zero)
        addSubview(meters)
    }

    override func layout() {
        super.layout()
        meters.frame = bounds
    }

    private func updateMeters() {
        switch part {
        case .cells:
            meters.bars = headers.enumerated().flatMap { hi, h -> [MeterBarsView.Bar] in
                let x = x0 + CGFloat(h.columns.lowerBound) * columnWidth
                let w = columnWidth * CGFloat(h.columns.count)
                let levels = hi < headerLevels.count ? headerLevels[hi] : []
                return levels.prefix(2).enumerated().map { i, level in
                    MeterBarsView.Bar(rect: NSRect(x: x + 14, y: 19 + CGFloat(i) * 5, width: w - 28, height: 4), level: level)
                }
            }
        case .labels:
            // During preview: received level replaces provenance (see `drawLabels`).
            meters.bars = listeningRow.map { r in
                [MeterBarsView.Bar(rect: NSRect(x: 220, y: headerHeight + CGFloat(r) * rowHeight + 13, width: 70, height: 5),
                                   level: listenLevel)]
            } ?? []
        }
    }

    required init?(coder: NSCoder) { fatalError("unused") }

    var rows: [GridRow] = [] { didSet { invalidateIntrinsicContentSize(); needsDisplay = true } }
    var columns = 4 { didSet { invalidateIntrinsicContentSize(); needsDisplay = true } }
    /// Column width: 46 for channels (two per pair), 92 for devices.
    var columnWidth: CGFloat = 46 { didSet { invalidateIntrinsicContentSize(); needsDisplay = true; updateMeters() } }
    var headers: [GridHeader] = [] { didSet { if headers != oldValue { needsDisplay = true; updateMeters() } } }
    /// Header meters (first two channels of each header).
    var headerLevels: [[Double?]] = [] { didSet { updateMeters() } }
    /// Click on a coupled group (row, header index).
    var onGroup: ((Int, Int) -> Void)?
    /// Click on one side of an uncoupled column (row, column, left side?).
    var onSide: ((Int, Int, Bool) -> Void)?
    /// Click on a header's link button (header index).
    var onCoupling: ((Int) -> Void)?
    /// Row preview-button click.
    var onListen: ((Int) -> Void)?
    /// Row removal-button click.
    var onRemove: ((Int) -> Void)?
    /// Previewed row and its level (dBFS).
    var listeningRow: Int? { didSet { needsDisplay = true; updateMeters() } }
    var listenLevel: Double? { didSet { updateMeters() } }
    /// Header meters (cells) or preview meter (labels).
    private let meters = MeterBarsView()

    private let headerHeight: CGFloat = 46
    private let rowHeight: CGFloat = 30
    static let labelWidth: CGFloat = 330
    private var labelWidth: CGFloat { Self.labelWidth }
    /// Left edge of column 0.
    private var x0: CGFloat { part == .cells ? 0 : labelWidth }
    /// Hovered target: row, column, side (nil: whole cell).
    private var hover: (row: Int, column: Int, left: Bool?)?
    private var hoverListen: Int?
    private var hoverRemove: Int?

    private func listenRect(row: Int) -> NSRect {
        NSRect(x: 2, y: headerHeight + CGFloat(row) * rowHeight + 5, width: 24, height: 20)
    }

    private func listenRow(at p: NSPoint) -> Int? {
        part == .labels ? rows.indices.first { listenRect(row: $0).contains(p) } : nil
    }

    /// Removal button, at the right end of the fixed labels (reachable whatever the scroll).
    private func removeRect(row: Int) -> NSRect {
        NSRect(x: labelWidth - 24, y: headerHeight + CGFloat(row) * rowHeight + 6, width: 18, height: 18)
    }

    private func removeRow(at p: NSPoint) -> Int? {
        part == .labels ? rows.indices.first { rows[$0].removable && removeRect(row: $0).contains(p) } : nil
    }

    override var isFlipped: Bool { true }

    /// Click acts even if window is not foreground (otherwise it merely activates it).
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override var intrinsicContentSize: NSSize {
        // Empty grid: two lines for the message in the labels part.
        let height = headerHeight + rowHeight * CGFloat(rows.isEmpty ? 2 : rows.count)
        return NSSize(width: part == .labels ? labelWidth : columnWidth * CGFloat(columns) + 8, height: height)
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(rect: bounds, options: [.mouseMoved, .mouseEnteredAndExited, .activeInKeyWindow], owner: self))
    }

    /// Header owning column `c`.
    private func header(of c: Int) -> Int? {
        headers.firstIndex { $0.columns.contains(c) }
    }

    /// Target under `p`: row, column, side (nil for a coupled group or a non-stereo source).
    private func cell(at p: NSPoint) -> (row: Int, column: Int, left: Bool?)? {
        guard part == .cells, p.x >= x0, p.y >= headerHeight else { return nil }
        let col = Int((p.x - x0) / columnWidth)
        let row = Int((p.y - headerHeight) / rowHeight)
        guard row < rows.count, col < columns, let h = header(of: col) else { return nil }
        if headers[h].coupled || rows[row].source.patchKind == "surround" {
            return (row, col, nil)
        }
        let top = p.y - headerHeight - CGFloat(row) * rowHeight < rowHeight / 2
        return (row, col, top)
    }

    private func linkRect(header h: Int) -> NSRect {
        let g = headers[h].columns
        return NSRect(x: x0 + CGFloat(g.upperBound) * columnWidth - 17, y: 1, width: 15, height: 14)
    }

    private func linkHeader(at p: NSPoint) -> Int? {
        part == .cells ? headers.indices.first { linkRect(header: $0).insetBy(dx: -3, dy: -2).contains(p) } : nil
    }

    override func mouseMoved(with event: NSEvent) {
        let point = convert(event.locationInWindow, from: nil)
        let newListen = listenRow(at: point)
        let newRemove = removeRow(at: point)
        if newListen != hoverListen || newRemove != hoverRemove {
            hoverListen = newListen
            hoverRemove = newRemove
            needsDisplay = true
        }
        let new = cell(at: point)
        if new.map({ "\($0.row)/\($0.column)/\(String(describing: $0.left))" })
            != hover.map({ "\($0.row)/\($0.column)/\(String(describing: $0.left))" }) {
            hover = new
            needsDisplay = true
        }
    }

    override func mouseExited(with event: NSEvent) {
        hover = nil
        hoverListen = nil
        hoverRemove = nil
        needsDisplay = true
    }

    /// Right-click row: preview, remove from matrix.
    override func menu(for event: NSEvent) -> NSMenu? {
        let p = convert(event.locationInWindow, from: nil)
        guard p.y >= headerHeight else { return nil }
        let r = Int((p.y - headerHeight) / rowHeight)
        guard r < rows.count else { return nil }
        let menu = NSMenu()
        menu.addItem(ClosureItem(listeningRow == r ? L("Stop Listening") : L("Listen on the computer's audio output")) { [weak self] in self?.onListen?(r) })
        if rows[r].removable {
            menu.addItem(ClosureItem(L("Remove from Grid")) { [weak self] in self?.onRemove?(r) })
        }
        return menu
    }

    override func mouseDown(with event: NSEvent) {
        let point = convert(event.locationInWindow, from: nil)
        if let r = listenRow(at: point) {
            onListen?(r)
        } else if let r = removeRow(at: point) {
            onRemove?(r)
        } else if let h = linkHeader(at: point) {
            onCoupling?(h)
        } else if let t = cell(at: point) {
            if let left = t.left {
                onSide?(t.row, t.column, left)
            } else if let h = header(of: t.column) {
                onGroup?(t.row, h)
            }
        }
    }

    private func text(_ s: String, _ rect: NSRect, font: NSFont, color: NSColor = .labelColor, center: Bool = false,
                      wrap: Bool = false) {
        let style = NSMutableParagraphStyle()
        style.alignment = center ? .center : .left
        style.lineBreakMode = wrap ? .byWordWrapping : .byTruncatingTail
        (s as NSString).draw(in: rect, withAttributes: [.font: font, .foregroundColor: color, .paragraphStyle: style])
    }

    override func draw(_ dirtyRect: NSRect) {
        // Column headers: title, state (meters: `meters` subview).
        for h in part == .cells ? headers : [] {
            let x = x0 + CGFloat(h.columns.lowerBound) * columnWidth
            let w = columnWidth * CGFloat(h.columns.count)
            text(h.title, NSRect(x: x, y: 0, width: w - 14, height: 16), font: Theme.small, color: .secondaryLabelColor, center: true)
            text(h.status, NSRect(x: x, y: 30, width: w, height: 14), font: Theme.small, color: .secondaryLabelColor, center: true)
        }
        for i in part == .cells ? Array(headers.indices) : [] {
            drawLink(linkRect(header: i), coupled: headers[i].coupled)
        }
        if rows.isEmpty {
            if part == .labels {
                text(L("No sources discovered. Choose the interface, or enter a channel below."),
                     NSRect(x: 0, y: headerHeight + 6, width: bounds.width - 8, height: 2 * rowHeight - 8),
                     font: Theme.body, color: .secondaryLabelColor, wrap: true)
            }
            return
        }
        for (r, row) in rows.enumerated() {
            let y = headerHeight + CGFloat(r) * rowHeight
            if r % 2 == 0 {
                NSColor.labelColor.withAlphaComponent(0.04).setFill()
                NSRect(x: 0, y: y, width: bounds.width, height: rowHeight).fill()
            }
            if part == .labels {
                drawLabels(row: row, r: r, y: y)
                continue
            }
            for (h, g) in headers.enumerated() {
                if g.coupled || row.source.patchKind == "surround" {
                    drawGroup(row: r, header: h, columns: g.columns, y: y, tag: row.groups[h], whole: g.coupled)
                } else {
                    for c in g.columns {
                        drawSides(row: r, column: c, y: y, sides: row.sides[c] ?? GridSides())
                    }
                }
            }
        }
    }

    /// Coupled group (or surround source on uncoupled columns): one target over the group's
    /// columns; patched: accent capsule with its label, or a dot for plain stereo.
    private func drawGroup(row r: Int, header h: Int, columns g: Range<Int>, y: CGFloat, tag: String?, whole: Bool) {
        let hovered = hover.map { $0.row == r && g.contains($0.column) } ?? false
        let cols = whole ? [g] : g.map { $0..<($0 + 1) }
        for span in cols {
            let left = x0 + CGFloat(span.lowerBound) * columnWidth + columnWidth / 2 - 9
            let right = x0 + CGFloat(span.upperBound - 1) * columnWidth + columnWidth / 2 + 9
            let wide = tag.map { !$0.isEmpty } ?? false && span.count == 1
            let box = NSRect(x: wide ? left - 10 : left, y: y + 6, width: right - left + (wide ? 20 : 0), height: 18)
            if let tag = tag {
                (hovered ? Theme.accent.withAlphaComponent(0.8) : Theme.accent).setFill()
                NSBezierPath(roundedRect: box, xRadius: 9, yRadius: 9).fill()
                if tag.isEmpty {
                    NSColor.white.setFill()
                    NSBezierPath(ovalIn: NSRect(x: box.midX - 3.5, y: box.midY - 3.5, width: 7, height: 7)).fill()
                } else {
                    text(tag, box.insetBy(dx: 2, dy: 2), font: Theme.small, color: .white, center: true)
                }
            } else {
                (hovered ? Theme.accent.withAlphaComponent(0.25) : Theme.emptyCell).setFill()
                NSBezierPath(roundedRect: box, xRadius: whole && span.count > 1 ? 9 : 5, yRadius: whole && span.count > 1 ? 9 : 5).fill()
            }
        }
    }

    /// Uncoupled column: left side on top, right side below, each a target.
    private func drawSides(row r: Int, column c: Int, y: CGFloat, sides: GridSides) {
        let x = x0 + CGFloat(c) * columnWidth + columnWidth / 2 - 11
        for (i, on, label) in [(0, sides.left, L("L")), (1, sides.right, L("R"))] {
            let box = NSRect(x: x, y: y + 3 + CGFloat(i) * 12.5, width: 22, height: 11.5)
            let hovered = hover.map { $0.row == r && $0.column == c && $0.left == (i == 0) } ?? false
            if on {
                (hovered ? Theme.accent.withAlphaComponent(0.8) : Theme.accent).setFill()
            } else {
                (hovered ? Theme.accent.withAlphaComponent(0.25) : Theme.emptyCell).setFill()
            }
            NSBezierPath(roundedRect: box, xRadius: 3, yRadius: 3).fill()
            text(label, box.offsetBy(dx: 0, dy: -1.5), font: Theme.tiny, color: on ? .white : .secondaryLabelColor, center: true)
        }
    }

    /// Link button: coupled pair (accent link) or uncoupled (broken link).
    private func drawLink(_ rect: NSRect, coupled: Bool) {
        let color: NSColor = coupled ? Theme.accent : .secondaryLabelColor
        if #available(macOS 11.0, *), let symbol = NSImage(systemSymbolName: "link", accessibilityDescription: coupled ? L("Unlink") : L("Link")) {
            let size = NSSize(width: 13, height: 13)
            let target = NSRect(x: rect.midX - size.width / 2, y: rect.midY - size.height / 2, width: size.width, height: size.height)
            NSImage(size: size, flipped: false) { bounds in
                symbol.draw(in: bounds)
                color.set()
                bounds.fill(using: .sourceAtop)
                return true
            }.draw(in: target)
        } else {
            text("∞", rect, font: Theme.small, color: color, center: true)
        }
        if !coupled {
            color.setStroke()
            let slash = NSBezierPath()
            slash.lineWidth = 1.2
            slash.move(to: NSPoint(x: rect.minX + 2, y: rect.maxY - 1))
            slash.line(to: NSPoint(x: rect.maxX - 2, y: rect.minY + 1))
            slash.stroke()
        }
    }

    /// Labels part of a row: preview button, channel, name, origin (or preview level).
    private func drawLabels(row: GridRow, r: Int, y: CGFloat) {
        let s = row.source
        drawListenButton(row: r)
        if row.removable {
            let rect = removeRect(row: r)
            let hot = hoverRemove == r
            if hot {
                NSColor.systemRed.withAlphaComponent(0.18).setFill()
                NSBezierPath(roundedRect: rect.insetBy(dx: -2, dy: -1), xRadius: 5, yRadius: 5).fill()
            }
            text("✕", rect, font: Theme.body, color: hot ? .systemRed : .secondaryLabelColor, center: true)
        }
        text("\(s.channel)", NSRect(x: 34, y: y + 8, width: 50, height: 16), font: Theme.mono)
        text(s.name.isEmpty ? "—" : s.name, NSRect(x: 88, y: y + 8, width: 130, height: 16), font: Theme.body)
        let originRect = NSRect(x: 220, y: y + 8, width: labelWidth - 250, height: 16)
        if listeningRow == r {
            // During preview: received level (`meters` subview, 70 pt) replaces provenance.
            text(L("listening"), NSRect(x: originRect.minX + 76, y: y + 8, width: 50, height: 16), font: Theme.small, color: Theme.accent)
        } else {
            let kind = s.kind == "surround" ? " · " + L("surround") : ""
            text(row.origin + kind, originRect, font: Theme.small, color: .secondaryLabelColor)
        }
    }

    /// Headphone button: filled while row is previewed.
    private func drawListenButton(row r: Int) {
        let rect = listenRect(row: r)
        let active = listeningRow == r
        let hovered = hoverListen == r
        if active || hovered {
            (active ? Theme.accent : NSColor.labelColor.withAlphaComponent(0.10)).setFill()
            NSBezierPath(roundedRect: rect, xRadius: 5, yRadius: 5).fill()
        }
        let color: NSColor = active ? .white : .secondaryLabelColor
        if #available(macOS 11.0, *), let symbol = NSImage(systemSymbolName: "headphones", accessibilityDescription: L("Listen on the computer's audio output")) {
            let size = NSSize(width: 14, height: 13)
            let target = NSRect(x: rect.midX - size.width / 2, y: rect.midY - size.height / 2, width: size.width, height: size.height)
            let tinted = NSImage(size: size, flipped: false) { bounds in
                symbol.draw(in: bounds)
                color.set()
                bounds.fill(using: .sourceAtop)
                return true
            }
            tinted.draw(in: target)
        } else {
            text("▶", rect.insetBy(dx: 0, dy: 2), font: Theme.small, color: color, center: true)
        }
    }
}

/// Input patch matrix: source labels fixed on the left, cells scrolling horizontally when the
/// columns do not fit (32 inputs and more), so the window keeps its width.
final class InputMatrixView: NSView {
    private let labels = InputGridView(part: .labels)
    private let cells = InputGridView(part: .cells)
    private let scroll = NSScrollView()
    private var scrollHeight: NSLayoutConstraint?

    var rows: [GridRow] = [] { didSet { both { $0.rows = rows }; updateHeight() } }
    var columns = 4 { didSet { cells.columns = columns } }
    var columnWidth: CGFloat = 46 { didSet { cells.columnWidth = columnWidth } }
    var headers: [GridHeader] = [] { didSet { cells.headers = headers } }
    var headerLevels: [[Double?]] = [] { didSet { cells.headerLevels = headerLevels } }
    var onGroup: ((Int, Int) -> Void)? { didSet { cells.onGroup = onGroup } }
    var onSide: ((Int, Int, Bool) -> Void)? { didSet { cells.onSide = onSide } }
    var onCoupling: ((Int) -> Void)? { didSet { cells.onCoupling = onCoupling } }
    var onListen: ((Int) -> Void)? { didSet { both { $0.onListen = onListen } } }
    var onRemove: ((Int) -> Void)? { didSet { both { $0.onRemove = onRemove } } }
    var listeningRow: Int? { didSet { both { $0.listeningRow = listeningRow } } }
    var listenLevel: Double? { didSet { labels.listenLevel = listenLevel } }

    private func both(_ f: (InputGridView) -> Void) {
        f(labels)
        f(cells)
    }

    override init(frame: NSRect) {
        super.init(frame: frame)
        labels.translatesAutoresizingMaskIntoConstraints = false
        cells.translatesAutoresizingMaskIntoConstraints = false
        scroll.translatesAutoresizingMaskIntoConstraints = false
        scroll.drawsBackground = false
        scroll.hasHorizontalScroller = true
        scroll.hasVerticalScroller = false
        scroll.autohidesScrollers = true
        scroll.verticalScrollElasticity = .none
        let clip = FlippedClipView()
        clip.drawsBackground = false
        scroll.contentView = clip
        scroll.documentView = cells
        addSubview(labels)
        addSubview(scroll)
        let height = scroll.heightAnchor.constraint(equalToConstant: 100)
        scrollHeight = height
        NSLayoutConstraint.activate([
            labels.leadingAnchor.constraint(equalTo: leadingAnchor),
            labels.topAnchor.constraint(equalTo: topAnchor),
            scroll.leadingAnchor.constraint(equalTo: labels.trailingAnchor),
            scroll.trailingAnchor.constraint(equalTo: trailingAnchor),
            scroll.topAnchor.constraint(equalTo: topAnchor),
            scroll.bottomAnchor.constraint(equalTo: bottomAnchor),
            height,
            labels.heightAnchor.constraint(equalTo: cells.heightAnchor),
            cells.topAnchor.constraint(equalTo: clip.topAnchor),
            cells.leadingAnchor.constraint(equalTo: clip.leadingAnchor),
        ])
        updateHeight()
    }

    required init?(coder: NSCoder) { fatalError("unused") }

    /// Grid height, plus room for a legacy (always visible) scroller.
    private func updateHeight() {
        let bar = NSScroller.preferredScrollerStyle == .legacy
            ? NSScroller.scrollerWidth(for: .regular, scrollerStyle: .legacy) : 0
        scrollHeight?.constant = cells.intrinsicContentSize.height + bar
    }
}

/// Clip view with top-left origin, so the cells stay aligned with the labels.
final class FlippedClipView: NSClipView {
    override var isFlipped: Bool { true }
}

/// Menu item executing a closure.
final class ClosureItem: NSMenuItem {
    private let run: () -> Void

    init(_ title: String, _ run: @escaping () -> Void) {
        self.run = run
        super.init(title: title, action: #selector(fire), keyEquivalent: "")
        target = self
    }

    required init(coder: NSCoder) { fatalError("unused") }

    @objc private func fire() { run() }
}
