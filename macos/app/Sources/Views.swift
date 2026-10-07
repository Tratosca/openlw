// OpenLW views: glass panel, meters, input matrix.
// AppKit only (10.13 minimum). Liquid Glass on macOS 26+, NSVisualEffectView below.

import AppKit

enum Theme {
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
    var levels: [Double?] = [] { didSet { needsDisplay = true } }

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

/// Patch drawn on a row: first column, column count, label (“L”, “R”, “L+R”, “8”, or none).
struct GridPatch: Equatable {
    let column: Int
    let span: Int
    let tag: String

    func covers(_ c: Int) -> Bool { c >= column && c < column + span }
}

/// Column-header group: title, meters, and state over a column range.
struct GridHeader {
    let columns: Range<Int>
    let title: String
    let levels: [Double?]
    let status: String
}

/// Input matrix row: source (discovered/manual) and optional patch.
struct GridRow {
    let source: DiscoveredSource
    /// Patch, or nil.
    let patch: GridPatch?
    /// Transmitting terminal, or row provenance if source not advertised.
    let origin: String
    /// Removable row (manual channel or configured unadvertised stream).
    let removable: Bool
}

/// Input patch matrix: source rows; columns are device channels (duplex layout) or input
/// devices (multi layout).
final class InputGridView: NSView {
    var rows: [GridRow] = [] { didSet { invalidateIntrinsicContentSize(); needsDisplay = true } }
    var columns = 4 { didSet { invalidateIntrinsicContentSize(); needsDisplay = true } }
    /// Column width: 46 for channels (two per pair), 92 for devices.
    var columnWidth: CGFloat = 46 { didSet { invalidateIntrinsicContentSize(); needsDisplay = true } }
    var headers: [GridHeader] = [] { didSet { needsDisplay = true } }
    /// Cell click (row, column, click point in view coordinates).
    var onToggle: ((Int, Int, NSPoint) -> Void)?
    /// Row preview-button click.
    var onListen: ((Int) -> Void)?
    /// Row removal-button click.
    var onRemove: ((Int) -> Void)?
    /// Previewed row and its level (dBFS).
    var listeningRow: Int? { didSet { needsDisplay = true } }
    var listenLevel: Double? { didSet { needsDisplay = true } }

    private let headerHeight: CGFloat = 46
    private let rowHeight: CGFloat = 30
    private let labelWidth: CGFloat = 330
    private var hover: (Int, Int)?
    private var hoverListen: Int?
    private var hoverRemove: Int?

    private func listenRect(row: Int) -> NSRect {
        NSRect(x: 2, y: headerHeight + CGFloat(row) * rowHeight + 5, width: 24, height: 20)
    }

    private func listenRow(at p: NSPoint) -> Int? {
        rows.indices.first { listenRect(row: $0).contains(p) }
    }

    private func removeRect(row: Int) -> NSRect {
        NSRect(x: labelWidth + columnWidth * CGFloat(columns) + 10, y: headerHeight + CGFloat(row) * rowHeight + 6,
               width: 18, height: 18)
    }

    private func removeRow(at p: NSPoint) -> Int? {
        rows.indices.first { rows[$0].removable && removeRect(row: $0).contains(p) }
    }

    override var isFlipped: Bool { true }

    /// Click acts even if window is not foreground (otherwise it merely activates it).
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override var intrinsicContentSize: NSSize {
        NSSize(width: max(680, labelWidth + columnWidth * CGFloat(columns) + 40), height: headerHeight + rowHeight * CGFloat(max(rows.count, 1)))
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(rect: bounds, options: [.mouseMoved, .mouseEnteredAndExited, .activeInKeyWindow], owner: self))
    }

    private func cell(at p: NSPoint) -> (Int, Int)? {
        guard p.x >= labelWidth, p.y >= headerHeight else { return nil }
        let col = Int((p.x - labelWidth) / columnWidth)
        let row = Int((p.y - headerHeight) / rowHeight)
        guard row < rows.count, col < columns else { return nil }
        return (row, col)
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
        if new.map({ [$0.0, $0.1] }) != hover.map({ [$0.0, $0.1] }) {
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
        menu.addItem(ClosureItem(listeningRow == r ? L("Stop Listening") : L("Listen")) { [weak self] in self?.onListen?(r) })
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
        } else if let (r, c) = cell(at: point) {
            onToggle?(r, c, point)
        }
    }

    private func text(_ s: String, _ rect: NSRect, font: NSFont, color: NSColor = .labelColor, center: Bool = false) {
        let style = NSMutableParagraphStyle()
        style.alignment = center ? .center : .left
        style.lineBreakMode = .byTruncatingTail
        (s as NSString).draw(in: rect, withAttributes: [.font: font, .foregroundColor: color, .paragraphStyle: style])
    }

    override func draw(_ dirtyRect: NSRect) {
        // Column headers: title, meters (first two channels), state.
        for h in headers {
            let x = labelWidth + CGFloat(h.columns.lowerBound) * columnWidth
            let w = columnWidth * CGFloat(h.columns.count)
            text(h.title, NSRect(x: x, y: 0, width: w, height: 16), font: Theme.small, color: .secondaryLabelColor, center: true)
            let mrect = NSRect(x: x + 14, y: 19, width: w - 28, height: 9)
            for (i, level) in h.levels.prefix(2).enumerated() {
                let y = mrect.minY + CGFloat(i) * 5
                NSColor.quaternaryLabelColor.setFill()
                NSRect(x: mrect.minX, y: y, width: mrect.width, height: 4).fill()
                if let db = level, db > -60 {
                    (db > -6 ? Theme.meterRed : (db > -18 ? Theme.meterAmber : Theme.meterGreen)).setFill()
                    NSRect(x: mrect.minX, y: y, width: mrect.width * CGFloat((db + 60) / 60), height: 4).fill()
                }
            }
            text(h.status, NSRect(x: x, y: 30, width: w, height: 14), font: Theme.small, color: .tertiaryLabelColor, center: true)
        }
        if rows.isEmpty {
            text(L("No sources discovered. Choose the interface, or enter a channel below."),
                 NSRect(x: 0, y: headerHeight + 6, width: bounds.width, height: 18), font: Theme.body, color: .secondaryLabelColor)
            return
        }
        for (r, row) in rows.enumerated() {
            let y = headerHeight + CGFloat(r) * rowHeight
            if r % 2 == 0 {
                NSColor.labelColor.withAlphaComponent(0.04).setFill()
                NSRect(x: 0, y: y, width: bounds.width, height: rowHeight).fill()
            }
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
            let originRect = NSRect(x: 220, y: y + 8, width: labelWidth - 226, height: 16)
            if listeningRow == r {
                // During preview: received level replaces provenance.
                let track = NSRect(x: originRect.minX, y: y + 13, width: 70, height: 5)
                NSColor.quaternaryLabelColor.setFill()
                track.fill()
                if let db = listenLevel, db > -60 {
                    (db > -6 ? Theme.meterRed : (db > -18 ? Theme.meterAmber : Theme.meterGreen)).setFill()
                    NSRect(x: track.minX, y: track.minY, width: track.width * CGFloat((db + 60) / 60), height: track.height).fill()
                }
                text(L("listening"), NSRect(x: track.maxX + 6, y: y + 8, width: 50, height: 16), font: Theme.small, color: Theme.accent)
            } else {
                let kind = s.kind == "surround" ? L(" · surround") : ""
                text(row.origin + kind, originRect, font: Theme.small, color: .secondaryLabelColor)
            }
            for c in 0..<columns where !(row.patch?.covers(c) ?? false) {
                let x = labelWidth + CGFloat(c) * columnWidth
                let box = NSRect(x: x + columnWidth / 2 - 9, y: y + 6, width: 18, height: 18)
                let hovered = hover.map { $0.0 == r && $0.1 == c } ?? false
                (hovered ? Theme.accent.withAlphaComponent(0.25) : NSColor.labelColor.withAlphaComponent(0.08)).setFill()
                NSBezierPath(roundedRect: box, xRadius: 5, yRadius: 5).fill()
            }
            // Patch: one capsule over its columns, with its label (or a dot for stereo).
            if let p = row.patch, p.column < columns {
                let span = min(p.span, columns - p.column)
                // A labelled single-column patch widens to fit its label (“L+R”).
                let half: CGFloat = span == 1 && !p.tag.isEmpty ? min(columnWidth / 2 - 4, 20) : 9
                let x0 = labelWidth + CGFloat(p.column) * columnWidth + columnWidth / 2 - half
                let x1 = labelWidth + CGFloat(p.column + span - 1) * columnWidth + columnWidth / 2 + half
                let box = NSRect(x: x0, y: y + 6, width: x1 - x0, height: 18)
                let hovered = hover.map { $0.0 == r && p.covers($0.1) } ?? false
                (hovered ? Theme.accent.withAlphaComponent(0.8) : Theme.accent).setFill()
                NSBezierPath(roundedRect: box, xRadius: 9, yRadius: 9).fill()
                if p.tag.isEmpty {
                    NSColor.white.setFill()
                    NSBezierPath(ovalIn: NSRect(x: box.midX - 3.5, y: box.midY - 3.5, width: 7, height: 7)).fill()
                } else {
                    text(p.tag, box.insetBy(dx: 2, dy: 2), font: Theme.small, color: .white, center: true)
                }
            }
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
        if #available(macOS 11.0, *), let symbol = NSImage(systemSymbolName: "headphones", accessibilityDescription: L("Listen")) {
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
