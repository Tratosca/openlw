// Main window: Livewire network connection, input patch matrix, transmitted outputs.
// Network service (LaunchDaemon) is hidden: app presents network, channels, and device.
// Poll status at 5 Hz; sources, configuration, interfaces, and Mac devices every 2 s.

import AppKit

final class MainWindowController: NSWindowController, NSTextFieldDelegate, NSWindowDelegate {
    private let client: DaemonClient

    // Last known state.
    private var config = DaemonConfig()
    private var configLoaded = false
    private var meters = DeviceMeters()
    private var discovered: [DiscoveredSource] = []
    private var ifaces: [Iface] = []
    /// Manually entered channels, persisted between launches.
    private var manual: [DiscoveredSource] = MainWindowController.loadManual() {
        didSet { saveManual() }
    }
    private var reachable = false
    private var link = LinkStatus()
    /// Active session interface (for preview).
    private var statusIface: String { link.searching ? "" : link.iface }
    private var statusIP: String { link.searching ? "" : link.ipv4 }

    // Preview: one source at a time, identified by channel/type (row order changes).
    private let listener = Listener()
    private var listening: (channel: Int, kind: String)?
    private var busy: Set<String> = []

    // Header.
    private let stateDot = NSView()
    private let stateLabel = NSTextField(labelWithString: L("Connecting to the OpenLW service…"))
    private let ifacePopup = NSPopUpButton()
    private let advertiseCheck = NSButton(checkboxWithTitle: L("Advertise outputs on the network"), target: nil, action: nil)
    private let messageLabel = NSTextField(wrappingLabelWithString: "")

    // Livewire channels per direction (device pairs, 1–16).
    static let maxPairs = 16
    private let inCount = NSPopUpButton()
    private let outCount = NSPopUpButton()

    // Mac default input/output.
    private let macInputLabel = NSTextField(labelWithString: "")
    private let macOutputLabel = NSTextField(labelWithString: "")
    private let useInput = NSButton(title: L("Use OpenLW"), target: nil, action: nil)
    private let useOutput = NSButton(title: L("Use OpenLW"), target: nil, action: nil)

    // Inputs.
    private let namingCheck = NSButton(checkboxWithTitle: L("Name devices after their source, for example “OpenLW In - Studio A@Omnia One (ch. 2)”"),
                                       target: nil, action: nil)
    private let layoutPopup = NSPopUpButton()
    static let layouts = [("duplex", L("One multichannel “OpenLW” device (input and output)")),
                          ("multi", L("Several devices, “OpenLW In n” and “OpenLW Out n”"))]
    private let inputsHint = NSTextField(wrappingLabelWithString: "")
    private let inCountTitle = NSTextField(labelWithString: "")
    private let outCountTitle = NSTextField(labelWithString: "")
    private let outputsHint = NSTextField(wrappingLabelWithString: "")
    /// Width-change warning not to be shown again (UserDefaults).
    private static let widthWarningKey = "skipWidthWarning"
    private let grid = InputMatrixView()
    private let manualChannel = NSTextField()
    private let manualKind = NSPopUpButton()

    // Outputs.
    private let outputStack = NSStackView()
    private var outputRows: [OutputRow] = []

    // Advanced settings.
    private let advancedToggle = NSButton()
    private let advancedBody = NSStackView()
    private let terminalField = NSTextField()
    private let latencyPopup = NSPopUpButton()
    private let dscpPopup = NSPopUpButton()
    static let latencies = [("low", L("Low: ≈ 8 ms added, dedicated network")),
                            ("normal", L("Normal: ≈ 17 ms added")),
                            ("safe", L("Safe: ≈ 35 ms added, shared network or busy Mac"))]
    static let dscps = [(46, L("EF (46): Livewire default")), (34, L("AF41 (34): recommended for AES67")), (0, L("None (0)"))]
    private static let advancedKey = "advancedVisible"

    private var timers: [Timer] = []

    init(window: NSWindow, client: DaemonClient) {
        self.client = client
        super.init(window: window)
    }

    required init?(coder: NSCoder) { fatalError("unused") }

    convenience init(client: DaemonClient) {
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 860, height: 720),
                              styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
                              backing: .buffered, defer: false)
        window.title = "OpenLW"
        window.titlebarAppearsTransparent = true
        window.minSize = NSSize(width: 760, height: 560)
        window.setFrameAutosaveName("OpenLWMain")
        self.init(window: window, client: client)
        build()
        window.center()
    }

    // MARK: - Construction

    private func build() {
        guard let window = window else { return }
        let root = NSVisualEffectView()
        root.material = .sidebar
        root.blendingMode = .behindWindow
        root.state = .followsWindowActiveState
        window.contentView = root

        let scroll = NSScrollView()
        scroll.translatesAutoresizingMaskIntoConstraints = false
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        root.addSubview(scroll)

        let stack = NSStackView()
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 14
        stack.edgeInsets = NSEdgeInsets(top: 40, left: 18, bottom: 18, right: 18)
        stack.translatesAutoresizingMaskIntoConstraints = false
        let doc = FlippedView()
        doc.translatesAutoresizingMaskIntoConstraints = false
        doc.addSubview(stack)
        scroll.documentView = doc

        NSLayoutConstraint.activate([
            scroll.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            scroll.topAnchor.constraint(equalTo: root.topAnchor),
            scroll.bottomAnchor.constraint(equalTo: root.bottomAnchor),
            doc.leadingAnchor.constraint(equalTo: scroll.contentView.leadingAnchor),
            doc.trailingAnchor.constraint(equalTo: scroll.contentView.trailingAnchor),
            doc.topAnchor.constraint(equalTo: scroll.contentView.topAnchor),
            stack.leadingAnchor.constraint(equalTo: doc.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: doc.trailingAnchor),
            stack.topAnchor.constraint(equalTo: doc.topAnchor),
            stack.bottomAnchor.constraint(equalTo: doc.bottomAnchor),
        ])

        for panel in [buildHeader(), buildDevice(), buildInputs(), buildOutputs(), buildAdvanced()] {
            stack.addArrangedSubview(panel)
            panel.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -36).isActive = true
        }
        updateLayoutTexts(multi: false)
    }

    private func buildHeader() -> GlassPanel {
        let panel = GlassPanel(title: L("Livewire Network"))
        stateDot.wantsLayer = true
        stateDot.layer?.cornerRadius = 5
        stateDot.layer?.backgroundColor = NSColor.systemGray.cgColor
        stateDot.translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([stateDot.widthAnchor.constraint(equalToConstant: 10), stateDot.heightAnchor.constraint(equalToConstant: 10)])
        stateLabel.font = Theme.body
        stateLabel.lineBreakMode = .byTruncatingTail
        stateLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        let status = NSStackView(views: [stateDot, stateLabel])
        status.spacing = 8

        ifacePopup.target = self
        ifacePopup.action = #selector(ifaceChosen(_:))
        advertiseCheck.target = self
        advertiseCheck.action = #selector(advertiseToggled(_:))
        let ifaceLabel = NSTextField(labelWithString: L("Interface:"))
        ifaceLabel.font = Theme.body
        let controls = NSStackView(views: [ifaceLabel, ifacePopup, NSView(), advertiseCheck])
        controls.spacing = 8
        ifacePopup.widthAnchor.constraint(greaterThanOrEqualToConstant: 280).isActive = true

        let clock = NSTextField(labelWithString: L("Clock: the Mac's own. Drift relative to other devices is compensated automatically."))
        clock.font = Theme.small
        clock.textColor = .secondaryLabelColor

        messageLabel.font = Theme.small
        messageLabel.textColor = .systemRed
        messageLabel.isHidden = true

        let v = NSStackView(views: [status, controls, clock, messageLabel])
        v.orientation = .vertical
        v.alignment = .leading
        v.spacing = 10
        embed(v, in: panel.content)
        for sub in [status, controls] {
            sub.widthAnchor.constraint(equalTo: v.widthAnchor).isActive = true
        }
        messageLabel.widthAnchor.constraint(equalTo: v.widthAnchor).isActive = true
        return panel
    }

    private func buildInputs() -> GlassPanel {
        let panel = GlassPanel(title: L("Mac Inputs (Network to Mac)"))
        let hint = inputsHint
        hint.font = Theme.small
        hint.textColor = .secondaryLabelColor

        grid.translatesAutoresizingMaskIntoConstraints = false
        grid.onGroup = { [weak self] row, header in self?.toggleGroup(row: row, header: header) }
        grid.onSide = { [weak self] row, col, left in self?.toggleSide(row: row, column: col, left: left) }
        grid.onCoupling = { [weak self] header in self?.toggleCoupling(header: header) }
        grid.onListen = { [weak self] row in self?.toggleListen(row: row) }
        grid.onRemove = { [weak self] row in self?.removeRow(row) }


        let addLabel = NSTextField(labelWithString: L("Unadvertised source, channel:"))
        addLabel.font = Theme.body
        manualChannel.placeholderString = L("1 to 32766")
        manualChannel.font = Theme.mono
        manualChannel.target = self
        manualChannel.action = #selector(addManual(_:))
        manualChannel.widthAnchor.constraint(equalToConstant: 90).isActive = true
        manualKind.addItems(withTitles: [L("Stereo"), L("Backfeed (To Source)"), L("8-channel surround")])
        let add = NSButton(title: L("Add to Grid"), target: self, action: #selector(addManual(_:)))
        let addRow = NSStackView(views: [addLabel, manualChannel, manualKind, add])
        addRow.spacing = 8

        let countRow = countControls(inCount, title: inCountTitle, unit: "inputs",
                                     macLabel: macInputLabel, use: useInput, useAction: #selector(useOpenLWInput(_:)))
        let v = NSStackView(views: [countRow, hint, grid, addRow])
        v.orientation = .vertical
        v.alignment = .leading
        v.spacing = 10
        embed(v, in: panel.content)
        hint.widthAnchor.constraint(equalTo: v.widthAnchor).isActive = true
        countRow.widthAnchor.constraint(equalTo: v.widthAnchor).isActive = true
        grid.widthAnchor.constraint(equalTo: v.widthAnchor).isActive = true
        return panel
    }

    private func buildDevice() -> GlassPanel {
        let panel = GlassPanel(title: L("Audio Device"))
        let label = NSTextField(labelWithString: L("Layout in macOS:"))
        label.font = Theme.body
        layoutPopup.addItems(withTitles: Self.layouts.map { $0.1 })
        layoutPopup.target = self
        layoutPopup.action = #selector(layoutChosen(_:))
        let row = NSStackView(views: [label, layoutPopup])
        row.spacing = 8
        namingCheck.target = self
        namingCheck.action = #selector(namingToggled(_:))
        let hint = NSTextField(wrappingLabelWithString: L("One device: every source goes to channels of “OpenLW”, as with a multichannel sound card; suits applications that use one device for input and output. Several devices: each source gets its own device, as wide as the source (1 channel when uncoupled, 8 for surround); suits applications that pick one input, such as video calls. Channels always carry the name of their source (Audio MIDI Setup, Logic…). After changing the layout or the names, select the device again in applications that find it by name, such as Audacity."))
        hint.font = Theme.small
        hint.textColor = .secondaryLabelColor
        let v = NSStackView(views: [row, namingCheck, hint])
        v.orientation = .vertical
        v.alignment = .leading
        v.spacing = 10
        embed(v, in: panel.content)
        hint.widthAnchor.constraint(equalTo: v.widthAnchor).isActive = true
        return panel
    }

    @objc private func layoutChosen(_ sender: NSPopUpButton) {
        let value = Self.layouts[max(0, sender.indexOfSelectedItem)].0
        guard value != config.layout, let window = window else { return }
        let alert = NSAlert()
        alert.messageText = value == "multi" ? L("Switch to several devices?") : L("Switch to one “OpenLW” device?")
        alert.informativeText = L("Patches move between input pair n and device n (a mono patch on input c goes to device ⌈c/2⌉); those that no longer fit are released. Audio on OpenLW devices stops for a moment while macOS reloads them.")
        alert.addButton(withTitle: L("Switch"))
        alert.addButton(withTitle: L("Cancel"))
        alert.beginSheetModal(for: window) { [weak self] response in
            guard let self = self else { return }
            if response == .alertFirstButtonReturn {
                self.mutate(["cmd": "set_device_layout", "layout": value])
            } else {
                self.applyConfig(self.config)
            }
        }
    }

    private func buildOutputs() -> GlassPanel {
        let panel = GlassPanel(title: L("Mac Outputs (Mac to Network)"))
        let hint = outputsHint
        hint.font = Theme.small
        hint.textColor = .secondaryLabelColor
        outputStack.orientation = .vertical
        outputStack.alignment = .leading
        outputStack.spacing = 6
        let countRow = countControls(outCount, title: outCountTitle, unit: "outputs",
                                     macLabel: macOutputLabel, use: useOutput, useAction: #selector(useOpenLWOutput(_:)))
        let v = NSStackView(views: [countRow, hint, outputStack])
        v.orientation = .vertical
        v.alignment = .leading
        v.spacing = 10
        embed(v, in: panel.content)
        hint.widthAnchor.constraint(equalTo: v.widthAnchor).isActive = true
        countRow.widthAnchor.constraint(equalTo: v.widthAnchor).isActive = true
        rebuildOutputRows(count: 1, multi: false)
        return panel
    }

    private func buildAdvanced() -> GlassPanel {
        let panel = GlassPanel(title: L("Advanced Settings"))
        advancedToggle.setButtonType(.pushOnPushOff)
        advancedToggle.bezelStyle = .disclosure
        advancedToggle.title = ""
        advancedToggle.target = self
        advancedToggle.action = #selector(advancedToggled(_:))
        let toggleLabel = NSTextField(labelWithString: L("Advertised name, receive latency, network priority"))
        toggleLabel.font = Theme.small
        toggleLabel.textColor = .secondaryLabelColor
        let toggleRow = NSStackView(views: [advancedToggle, toggleLabel])
        toggleRow.spacing = 6

        func row(_ label: String, _ control: NSView, _ hint: String) -> NSStackView {
            let l = NSTextField(labelWithString: label)
            l.font = Theme.body
            l.alignment = .right
            l.widthAnchor.constraint(equalToConstant: 170).isActive = true
            let h = NSTextField(wrappingLabelWithString: hint)
            h.font = Theme.small
            h.textColor = .secondaryLabelColor
            let line = NSStackView(views: [l, control])
            line.spacing = 8
            let v = NSStackView(views: [line, h])
            v.orientation = .vertical
            v.alignment = .leading
            v.spacing = 4
            h.leadingAnchor.constraint(equalTo: control.leadingAnchor).isActive = true
            h.widthAnchor.constraint(lessThanOrEqualToConstant: 560).isActive = true
            return v
        }
        terminalField.placeholderString = Host.current().localizedName ?? L("computer name")
        terminalField.widthAnchor.constraint(equalToConstant: 260).isActive = true
        terminalField.target = self
        terminalField.action = #selector(terminalNameEntered(_:))
        latencyPopup.addItems(withTitles: Self.latencies.map { $0.1 })
        latencyPopup.target = self
        latencyPopup.action = #selector(latencyChosen(_:))
        dscpPopup.addItems(withTitles: Self.dscps.map { $0.1 })
        dscpPopup.target = self
        dscpPopup.action = #selector(dscpChosen(_:))

        advancedBody.orientation = .vertical
        advancedBody.alignment = .leading
        advancedBody.spacing = 12
        for v in [
            row(L("Advertised Mac name:"), terminalField,
                L("Name shown by other Livewire devices, 32 characters at most, accents replaced. Empty: computer name. Press Return to apply.")),
            row(L("Receive latency:"), latencyPopup,
                L("Buffering added to that of the recording application. The lower the latency, the more likely a network or Mac delay causes a brief dropout.")),
            row(L("Network priority (DSCP):"), dscpPopup,
                L("Marking of the audio streams transmitted by the Mac. Choose the value expected by the QoS policy of your switches.")),
        ] {
            advancedBody.addArrangedSubview(v)
        }
        let visible = UserDefaults.standard.bool(forKey: Self.advancedKey)
        advancedToggle.state = visible ? .on : .off
        advancedBody.isHidden = !visible

        let v = NSStackView(views: [toggleRow, advancedBody])
        v.orientation = .vertical
        v.alignment = .leading
        v.spacing = 12
        embed(v, in: panel.content)
        return panel
    }

    @objc private func advancedToggled(_ sender: NSButton) {
        advancedBody.isHidden = sender.state == .off
        UserDefaults.standard.set(sender.state == .on, forKey: Self.advancedKey)
    }

    @objc private func terminalNameEntered(_ sender: NSTextField) {
        let name = sender.stringValue.trimmingCharacters(in: .whitespaces)
        guard name != config.terminalName else { return }
        mutate(["cmd": "set_advanced", "terminal_name": name])
    }

    @objc private func latencyChosen(_ sender: NSPopUpButton) {
        let value = Self.latencies[max(0, sender.indexOfSelectedItem)].0
        guard value != config.latency else { return }
        mutate(["cmd": "set_advanced", "latency": value])
    }

    @objc private func dscpChosen(_ sender: NSPopUpButton) {
        let dscp = Self.dscps[max(0, sender.indexOfSelectedItem)].0
        guard dscp << 2 != config.tos else { return }
        mutate(["cmd": "set_advanced", "dscp": dscp])
    }

    /// Panel channel-count row with Mac's default device for that direction.
    private func countControls(_ popup: NSPopUpButton, title: NSTextField, unit: String, macLabel: NSTextField,
                               use: NSButton, useAction: Selector) -> NSStackView {
        title.font = Theme.body
        popup.addItems(withTitles: (1...Self.maxPairs).map { "\($0)" })
        popup.identifier = NSUserInterfaceItemIdentifier(unit)
        popup.target = self
        popup.action = #selector(channelCountChosen(_:))
        macLabel.font = Theme.small
        macLabel.textColor = .secondaryLabelColor
        macLabel.lineBreakMode = .byTruncatingTail
        macLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        use.target = self
        use.action = useAction
        use.controlSize = .small
        let row = NSStackView(views: [title, popup, NSView(), macLabel, use])
        row.spacing = 8
        return row
    }

    private func embed(_ view: NSView, in container: NSView) {
        view.translatesAutoresizingMaskIntoConstraints = false
        container.addSubview(view)
        NSLayoutConstraint.activate([
            view.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            view.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            view.topAnchor.constraint(equalTo: container.topAnchor),
            view.bottomAnchor.constraint(equalTo: container.bottomAnchor),
        ])
    }

    /// Output rows: one per pair (duplex layout) or per output device (multi layout).
    private func rebuildOutputRows(count: Int, multi: Bool) {
        outputRows.forEach { $0.removeFromSuperview() }
        outputRows = (0..<count).map { i in
            let row = OutputRow(pair: multi ? [1, 2] : [2 * i + 1, 2 * i + 2], device: multi ? i + 1 : nil)
            row.onApply = { [weak self] r in self?.applyOutput(r) }
            return row
        }
        outputRows.forEach(outputStack.addArrangedSubview)
    }

    /// Count-menu titles and panel hints for the layout.
    private func updateLayoutTexts(multi: Bool) {
        inCountTitle.stringValue = multi ? L("Input devices:") : L("Received Livewire channels:")
        outCountTitle.stringValue = multi ? L("Output devices:") : L("Transmitted Livewire channels:")
        for (popup, input) in [(inCount, true), (outCount, false)] {
            for (i, item) in popup.itemArray.enumerated() {
                let n = "\(i + 1)", two = "\(2 * (i + 1))"
                item.title = multi ? (i == 0 ? L("1 device") : L("%@ devices", n))
                    : input ? (i == 0 ? L("1 channel (2 inputs)") : L("%@ channels (%@ inputs)", n, two))
                    : (i == 0 ? L("1 channel (2 outputs)") : L("%@ channels (%@ outputs)", n, two))
            }
        }
        inputsHint.stringValue = multi
            ? L("Click a cell to send the source to that device in stereo; click again to release it. The link button above a device uncouples it: the device becomes mono and each cell offers the left (L) and right (R) sides of the source, both for L+R. Applications record from “OpenLW In n”. The headphone button plays the source on the Mac's output without patching it.")
            : L("Click a cell to send the source in stereo to that pair of the OpenLW device; click again to release it. The link button above a pair uncouples it: each input then offers the left (L) and right (R) sides of the source, both for L+R, and a source can feed several inputs. Applications record from “OpenLW”. The headphone button plays the source on the Mac's output without patching it.")
        outputsHint.stringValue = multi
            ? L("Choose “OpenLW Out n” as the output of the Mac or of your application. Each device is transmitted on the Livewire channel of your choice, under the name entered. Changes apply while Transmit is checked.")
            : L("Choose “OpenLW” as the output of the Mac or of your application. Each output pair is transmitted on the Livewire channel of your choice, under the name entered. Changes apply while Transmit is checked.")
    }

    // MARK: - Polling

    func start() {
        window?.delegate = self
        refreshSlow()
        refreshStatus()
        timers = [
            Timer.scheduledTimer(withTimeInterval: 0.2, repeats: true) { [weak self] _ in self?.refreshStatus() },
            Timer.scheduledTimer(withTimeInterval: 2.0, repeats: true) { [weak self] _ in self?.refreshSlow() },
        ]
        timers.forEach { RunLoop.main.add($0, forMode: .common) }
    }

    private var shownOnce = false

    /// First activation: no active field (macOS would scroll to first text field,
    /// the advertised name at window bottom); display from top.
    func windowDidBecomeKey(_ notification: Notification) {
        guard !shownOnce else { return }
        shownOnce = true
        window?.makeFirstResponder(nil)
        (window?.contentView?.subviews.first as? NSScrollView)?.documentView?.scroll(.zero)
    }

    /// Send `request` unless a request of the same type is already active.
    private func poll(_ key: String, _ request: [String: Any], _ handle: @escaping ([String: Any]) -> Void) {
        guard !busy.contains(key) else { return }
        busy.insert(key)
        client.call(request) { [weak self] result in
            guard let self = self else { return }
            self.busy.remove(key)
            switch result {
            case .success(let reply):
                handle(reply)
            case .failure(.unreachable):
                self.setReachable(false)
            case .failure:
                break // Command absent on older daemon: information remains empty
            }
        }
    }

    private func refreshStatus() {
        poll("status", ["cmd": "status"]) { [weak self] reply in
            guard let self = self, let status = reply["status"] as? [String: Any] else { return }
            self.setReachable(true)
            self.meters = DeviceMeters(status)
            let next = LinkStatus(status)
            if self.listening != nil && (next.iface != self.link.iface || next.ipv4 != self.link.ipv4 || next.searching) {
                self.stopListening() // Interface changed: membership no longer valid
            }
            let changed = next.searching != self.link.searching || next.iface != self.link.iface || next.auto != self.link.auto
            self.link = next
            self.showLink()
            if changed { self.updateIfacePopup() }
            self.updateMeters()
        }
    }

    private func refreshSlow() {
        updateMacDevices()
        poll("config", ["cmd": "config"]) { [weak self] reply in
            guard let self = self, let c = reply["config"] as? [String: Any] else { return }
            self.applyConfig(DaemonConfig(c))
        }
        poll("sources", ["cmd": "sources"]) { [weak self] reply in
            guard let self = self else { return }
            self.discovered = (reply["sources"] as? [[String: Any]] ?? []).compactMap(DiscoveredSource.init)
            self.updateGrid()
        }
        poll("ifaces", ["cmd": "ifaces"]) { [weak self] reply in
            guard let self = self else { return }
            self.ifaces = (reply["ifaces"] as? [[String: Any]] ?? []).compactMap(Iface.init)
            self.updateIfacePopup()
        }
    }

    private func setReachable(_ ok: Bool) {
        guard ok != reachable || !ok else { return }
        reachable = ok
        if !ok {
            stateDot.layer?.backgroundColor = NSColor.systemRed.cgColor
            stateLabel.stringValue = DaemonError.unreachable.message
        }
    }

    /// Status line: active interface or network search.
    private func showLink() {
        if link.searching {
            stateDot.layer?.backgroundColor = NSColor.systemOrange.cgColor
            stateLabel.stringValue = link.auto
                ? L("Searching for the Livewire network. Connect the Mac to the Livewire network, or choose the interface.")
                : L("Interface “%@” unavailable. Connect it, or choose Automatic.", config.iface)
            return
        }
        stateDot.layer?.backgroundColor = NSColor.systemGreen.cgColor
        let name = link.friendly == link.iface ? link.iface : "\(link.friendly) (\(link.iface))"
        stateLabel.stringValue = L("Connected to the Livewire network via %@ · %@", name, link.ipv4)
            + (link.auto ? L(" · interface chosen automatically") : "")
    }

    /// Mac default input/output and buttons to select “OpenLW”.
    private func updateMacDevices() {
        for (input, label, button) in [(true, macInputLabel, useInput), (false, macOutputLabel, useOutput)] {
            let lw = MacAudio.livewireDevice(input: input)
            let current = MacAudio.defaultDevice(input: input)
            let name = current.map(MacAudio.name) ?? L("none")
            label.stringValue = input ? L("Mac input: %@", name) : L("Mac output: %@", name)
            button.isHidden = lw == nil || current == lw
        }
    }

    // MARK: - Display updates

    private func applyConfig(_ c: DaemonConfig) {
        let rowsChanged = !configLoaded || c.channelsToNet != config.channelsToNet || c.layout != config.layout
        config = c
        configLoaded = true
        advertiseCheck.state = c.advertise ? .on : .off
        namingCheck.state = c.customNames ? .on : .off
        namingCheck.isEnabled = c.multi
        updateLayoutTexts(multi: c.multi)
        if let i = Self.layouts.firstIndex(where: { $0.0 == c.layout }) { layoutPopup.selectItem(at: i) }
        let editingName = (window?.firstResponder as? NSTextView)?.delegate === terminalField
        if !editingName { terminalField.stringValue = c.terminalName }
        if let i = Self.latencies.firstIndex(where: { $0.0 == c.latency }) { latencyPopup.selectItem(at: i) }
        if let i = Self.dscps.firstIndex(where: { $0.0 << 2 == c.tos }) {
            dscpPopup.selectItem(at: i)
        } else {
            dscpPopup.select(nil) // Value outside list (manually edited configuration)
        }
        if rowsChanged {
            rebuildOutputRows(count: max(1, c.multi ? c.outDevices : c.channelsToNet / 2), multi: c.multi)
        }
        grid.columns = c.multi ? c.inDevices : c.channelsFromNet
        grid.columnWidth = c.multi ? 92 : 46
        inCount.selectItem(at: min(Self.maxPairs, max(1, c.channelsFromNet / 2)) - 1)
        outCount.selectItem(at: min(Self.maxPairs, max(1, c.channelsToNet / 2)) - 1)
        for row in outputRows {
            row.show(outputPatch(row))
        }
        updateIfacePopup()
        updateGrid()
    }

    /// Menu: automatic selection, then Ethernet interfaces (plus configured interface if not Ethernet).
    private func updateIfacePopup() {
        let auto = config.autoIface
        let autoTitle: String
        if !auto {
            autoTitle = L("Automatic")
        } else if link.searching {
            autoTitle = L("Automatic · searching")
        } else {
            autoTitle = L("Automatic · %@", link.friendly)
        }
        var items: [(String, String)] = [(autoTitle, "auto")]
        var listed = ifaces.filter { $0.candidate || (!auto && $0.name == config.iface) }
        listed.sort { ($0.livewire ? 0 : 1, $0.name) < ($1.livewire ? 0 : 1, $1.name) }
        items += listed.map { ($0.title, $0.name) }
        if !auto && !ifaces.contains(where: { $0.name == config.iface }) {
            items.append((L("%@ (unavailable)", config.iface), config.iface))
        }
        let existing = ifacePopup.itemArray.filter { !$0.isSeparatorItem }.map { "\($0.title)|\($0.representedObject as? String ?? "")" }
        if existing != items.map({ "\($0.0)|\($0.1)" }) {
            ifacePopup.removeAllItems()
            for (i, (title, name)) in items.enumerated() {
                if i == 1 { ifacePopup.menu?.addItem(.separator()) }
                ifacePopup.addItem(withTitle: title)
                ifacePopup.lastItem?.representedObject = name
            }
        }
        let wanted = auto ? "auto" : config.iface
        if let item = ifacePopup.itemArray.first(where: { $0.representedObject as? String == wanted }) {
            ifacePopup.select(item)
        }
    }

    /// Matrix rows: discovered sources, configured unadvertised streams, then manual entries.
    private var gridRows: [GridRow] = []

    /// Configured received stream of a source, if any.
    private func inputPatch(channel: Int, kind: String) -> InputPatch? {
        config.inputs.first { $0.channel == channel && $0.kind == kind }
    }

    /// Header groups: pairs (duplex layout) or input devices (multi layout): columns, title,
    /// concatenated device channels (meters, route state), pair or device number.
    private func headerGroups() -> [(columns: Range<Int>, title: String, channels: [Int], number: Int)] {
        if config.multi {
            let widths = meters.inWidths.count == config.inDevices ? meters.inWidths
                : DaemonConfig.inWidths(config.inputs, uncoupled: config.uncoupled, devices: config.inDevices)
            return (1...max(1, config.inDevices)).map { n in
                (n - 1..<n, L("In %@", "\(n)"), DeviceMeters.channels(device: n, widths: widths), n)
            }
        }
        return stride(from: 1, through: config.channelsFromNet, by: 2).map { a in
            let b = min(a + 1, config.channelsFromNet)
            return (a - 1..<b, a == b ? L("Input %@", "\(a)") : L("Inputs %@-%@", "\(a)", "\(b)"), Array(a...b), (a + 1) / 2)
        }
    }

    private static func sideTag(_ from: [Int], stereo: Bool) -> String {
        switch (stereo, from) {
        case (true, [1]): return L("L")
        case (true, [2]): return L("R")
        case (true, [1, 2]): return L("L+R")
        default: return from.map(String.init).joined(separator: "+")
        }
    }

    /// Crosspoints of a source as drawn: coupled groups and uncoupled sides.
    private func gridCells(_ s: DiscoveredSource) -> (groups: [Int: String], sides: [Int: GridSides]) {
        guard let p = inputPatch(channel: s.channel, kind: s.patchKind), !p.taps.isEmpty else { return ([:], [:]) }
        let stereo = p.kind != "surround"
        var groups: [Int: String] = [:]
        var sides: [Int: GridSides] = [:]
        for (h, g) in headerGroups().enumerated() {
            // Taps of this group, by column (multi: the device's channel 1 for the sides).
            let taps = p.taps.filter { t in
                config.multi ? t.device == g.number : g.columns.contains(t.channel - 1)
            }
            guard !taps.isEmpty else { continue }
            if config.coupled(g.number) || !stereo {
                let plain = stereo && taps.count == 2 && taps.allSatisfy { t in
                    let first = config.multi ? 1 : g.columns.lowerBound + 1
                    return t.from == [t.channel - first + 1]
                }
                groups[h] = plain ? "" : stereo ? taps.map { Self.sideTag($0.from, stereo: true) }.joined(separator: "·")
                    : "\(taps.compactMap { $0.from.first }.min() ?? 1)-\(taps.compactMap { $0.from.first }.max() ?? 8)"
            } else {
                for t in taps {
                    let col = config.multi ? g.columns.lowerBound : t.channel - 1
                    sides[col] = GridSides(left: t.from.contains(1), right: t.from.contains(2))
                }
            }
        }
        return (groups, sides)
    }

    /// Configured transmitted stream of an output row.
    private func outputPatch(_ row: OutputRow) -> OutputPatch? {
        config.outputs.first { row.device != nil ? $0.device == row.device : ($0.device == nil && $0.deviceChannels == row.pair) }
    }

    private func updateGrid() {
        var rows: [GridRow] = []
        var seen = Set<String>()
        func add(_ s: DiscoveredSource, origin: String, removable: Bool) {
            let key = "\(s.channel)/\(s.patchKind)"
            guard !seen.contains(key) else { return }
            seen.insert(key)
            let cells = gridCells(s)
            rows.append(GridRow(source: s, groups: cells.groups, sides: cells.sides,
                                origin: origin, removable: removable))
        }
        for s in discovered { add(s, origin: s.terminal, removable: false) }
        for s in manual { add(s, origin: L("manual"), removable: true) }
        for p in config.inputs {
            guard let ch = p.channel else { continue }
            add(DiscoveredSource(channel: ch, name: "", kind: p.kind, terminal: ""), origin: L("not advertised"), removable: true)
        }
        gridRows = rows
        grid.rows = rows
        grid.listeningRow = listening.flatMap { l in rows.firstIndex { $0.source.channel == l.channel && $0.source.patchKind == l.kind } }
        updateMeters()
    }

    private func updateMeters() {
        grid.listenLevel = listening == nil ? nil : listener.takePeak()
        grid.headers = headerGroups().map { g in
            let primed = meters.inputsPrimed.first { !Set($0.key).isDisjoint(with: g.channels) }?.value
            return GridHeader(columns: g.columns, title: g.title,
                              levels: g.channels.map { DeviceMeters.peak(meters.fromNet, channels: [$0]) },
                              status: primed.map { $0 ? L("receiving audio") : L("waiting") } ?? L("free"),
                              coupled: config.coupled(g.number))
        }
        let outWidths = meters.outWidths.count == outputRows.count ? meters.outWidths : Array(repeating: 2, count: outputRows.count)
        for row in outputRows {
            let chs = row.device.map { DeviceMeters.channels(device: $0, widths: outWidths) } ?? row.pair
            row.meter.levels = chs.prefix(2).map { DeviceMeters.peak(meters.toNet, channels: [$0]) }
        }
    }

    private func show(_ error: DaemonError?) {
        messageLabel.stringValue = error?.message ?? ""
        messageLabel.isHidden = error == nil
    }

    // MARK: - Actions

    /// Modification request; returned configuration replaces displayed state.
    private func mutate(_ request: [String: Any]) {
        client.call(request) { [weak self] result in
            guard let self = self else { return }
            switch result {
            case .success(let reply):
                self.show(nil)
                if let c = reply["config"] as? [String: Any] { self.applyConfig(DaemonConfig(c)) }
            case .failure(let e):
                self.show(e)
                self.refreshSlow()
            }
        }
    }

    @objc private func ifaceChosen(_ sender: NSPopUpButton) {
        guard let name = sender.selectedItem?.representedObject as? String else { return }
        if name == "auto" ? config.autoIface : name == config.iface { return }
        mutate(["cmd": "set_iface", "iface": name])
    }

    @objc private func channelCountChosen(_ sender: NSPopUpButton) {
        let toNet = 2 * (outCount.indexOfSelectedItem + 1)
        let fromNet = 2 * (inCount.indexOfSelectedItem + 1)
        guard toNet != config.channelsToNet || fromNet != config.channelsFromNet else { return }
        let multi = config.multi
        let lostOut = config.outputs.filter { multi ? ($0.device ?? 0) > (toNet + 1) / 2 : ($0.deviceChannels?.max() ?? 0) > toNet }.count
        let lostIn = config.inputs.filter { p in
            !p.taps.isEmpty && p.taps.allSatisfy { multi ? ($0.device ?? 0) > (fromNet + 1) / 2 : $0.channel > fromNet }
        }.count
        if lostOut + lostIn > 0, let window = window {
            let alert = NSAlert()
            alert.messageText = multi ? L("Reduce the number of devices?") : L("Reduce the number of channels?")
            var lost: [String] = []
            if lostOut > 0 { lost.append(lostOut == 1 ? L("1 transmission stopped") : L("%@ transmissions stopped", "\(lostOut)")) }
            if lostIn > 0 { lost.append(lostIn == 1 ? L("1 source removed from the inputs") : L("%@ sources removed from the inputs", "\(lostIn)")) }
            alert.informativeText = L("%@. Audio on OpenLW devices stops for a moment.", lost.joined(separator: ", "))
            alert.addButton(withTitle: L("Reduce"))
            alert.addButton(withTitle: L("Cancel"))
            alert.beginSheetModal(for: window) { [weak self] response in
                guard let self = self else { return }
                if response == .alertFirstButtonReturn {
                    self.mutate(["cmd": "set_device_channels", "to_net": toNet, "from_net": fromNet])
                } else {
                    self.applyConfig(self.config)
                }
            }
            return
        }
        confirmCut(message: multi ? L("Change the number of devices?") : L("Change the number of channels?"),
                   onCancel: { [weak self] in self.map { $0.applyConfig($0.config) } }) { [weak self] in
            self?.mutate(["cmd": "set_device_channels", "to_net": toNet, "from_net": fromNet])
        }
    }

    /// Changes that recreate the shared region cut OpenLW audio briefly: ask first while an
    /// application uses an OpenLW device (or always, for `always`), unless the user opted out.
    private func confirmCut(message: String, detail: String? = nil, always: Bool = false,
                            onCancel: @escaping () -> Void = {}, _ proceed: @escaping () -> Void) {
        let skip = UserDefaults.standard.bool(forKey: Self.widthWarningKey)
        guard let window = window, !skip, always || MacAudio.livewireRunning() else {
            proceed()
            return
        }
        let alert = NSAlert()
        alert.messageText = message
        let cut = L("Audio on all OpenLW devices stops for a moment while macOS reloads them.")
        alert.informativeText = detail.map { $0 + " " + cut } ?? cut
        alert.showsSuppressionButton = true
        alert.suppressionButton?.title = L("Do not ask again")
        alert.addButton(withTitle: L("Continue"))
        alert.addButton(withTitle: L("Cancel"))
        alert.beginSheetModal(for: window) { response in
            if alert.suppressionButton?.state == .on {
                UserDefaults.standard.set(true, forKey: Self.widthWarningKey)
            }
            if response == .alertFirstButtonReturn { proceed() } else { onCancel() }
        }
    }

    @objc private func useOpenLWInput(_ sender: Any) { useOpenLW(input: true) }
    @objc private func useOpenLWOutput(_ sender: Any) { useOpenLW(input: false) }

    private func useOpenLW(input: Bool) {
        guard let lw = MacAudio.livewireDevice(input: input) else {
            show(.refused(L("the OpenLW device cannot be found. Reinstall OpenLW.")))
            return
        }
        if !MacAudio.setDefault(lw, input: input) {
            show(.refused(input ? L("macOS refused to change the default input.") : L("macOS refused to change the default output.")))
        }
        updateMacDevices()
    }

    @objc private func advertiseToggled(_ sender: NSButton) {
        mutate(["cmd": "set_advertise", "advertise": sender.state == .on])
    }

    @objc private func addManual(_ sender: Any) {
        guard let ch = Int(manualChannel.stringValue.trimmingCharacters(in: .whitespaces)), (1...32766).contains(ch) else {
            show(.refused(L("invalid channel. Enter a number from 1 to 32766.")))
            return
        }
        let kind = ["stereo", "backfeed", "surround"][max(0, manualKind.indexOfSelectedItem)]
        if !manual.contains(where: { $0.channel == ch && $0.kind == kind }) {
            manual.append(DiscoveredSource(channel: ch, name: "", kind: kind, terminal: ""))
        }
        manualChannel.stringValue = ""
        show(nil)
        updateGrid()
    }

    /// Stream fields of a patch request for source `s`.
    private func streamFields(_ s: DiscoveredSource) -> [String: Any] {
        ["channel": s.channel, "kind": s.patchKind]
    }

    /// Click on a coupled group (or on a surround source): patch the source in stereo (or its
    /// 8 channels from the group start), or release it from the group.
    private func toggleGroup(row: Int, header h: Int) {
        let groups = headerGroups()
        guard row < gridRows.count, h < groups.count else { return }
        let r = gridRows[row]
        let s = r.source
        let g = groups[h]
        let surround = s.patchKind == "surround"
        let n = g.number
        if r.groups[h] != nil || r.sides.keys.contains(where: g.columns.contains) {
            // Release: the group's inputs, or the whole surround block.
            if config.multi {
                let w = DaemonConfig.inWidths(config.inputs, uncoupled: config.uncoupled, devices: config.inDevices)[n - 1]
                send(["cmd": "unpatch_input", "device": n, "device_channels": Array(1...w)], source: s, device: n, width: nil)
            } else {
                let block = surround ? (inputPatch(channel: s.channel, kind: s.patchKind)?.taps.map(\.channel) ?? [])
                    : Array((g.columns.lowerBound + 1)...g.columns.upperBound)
                mutate(["cmd": "unpatch_input", "device_channels": block])
            }
            return
        }
        var taps: [Tap]
        if config.multi {
            if surround && !config.coupled(n) {
                show(.refused(L("a surround source needs a coupled device. Couple “OpenLW In %@” first.", "\(n)")))
                return
            }
            taps = (1...(surround ? 8 : 2)).map { Tap(device: n, channel: $0, from: [$0]) }
            if !config.coupled(n) { taps = [Tap(device: n, channel: 1, from: [1, 2])] }
        } else {
            let first = g.columns.lowerBound + 1
            let width = surround ? 8 : min(2, g.columns.count)
            guard first + width - 1 <= config.channelsFromNet else {
                show(.refused(L("a surround source occupies 8 inputs. Choose a pair from 1-2 to %@-%@.", "\(config.channelsFromNet - 7)", "\(config.channelsFromNet - 6)")))
                return
            }
            taps = (0..<width).map { Tap(device: nil, channel: first + $0, from: [$0 + 1]) }
        }
        var req = streamFields(s)
        req["cmd"] = "patch_input"
        req["taps"] = taps.map(\.json)
        send(req, source: s, device: config.multi ? n : nil, width: taps.count)
    }

    /// Click on one side of an uncoupled column: add or remove that side of the source on
    /// this input (both sides: L+R).
    private func toggleSide(row: Int, column c: Int, left: Bool) {
        guard row < gridRows.count else { return }
        let s = gridRows[row].source
        let side = left ? 1 : 2
        let device: Int? = config.multi ? c + 1 : nil
        let channel = config.multi ? 1 : c + 1
        let current = inputPatch(channel: s.channel, kind: s.patchKind)?.taps
            .first { $0.device == device && $0.channel == channel }?.from ?? []
        let from = current.contains(side) ? current.filter { $0 != side } : (current + [side]).sorted()
        var req = streamFields(s)
        req["cmd"] = "patch_input"
        req["taps"] = [Tap(device: device, channel: channel, from: from).json]
        mutate(req)
    }

    /// Link button: couple or uncouple a pair (multi layout: a device, whose width changes).
    private func toggleCoupling(header h: Int) {
        let groups = headerGroups()
        guard h < groups.count else { return }
        let n = groups[h].number
        let coupled = config.coupled(n)
        let request: [String: Any] = ["cmd": "set_coupling", "pair": n, "coupled": !coupled]
        if config.multi {
            let detail = coupled ? L("“OpenLW In %@” becomes a mono device.", "\(n)")
                                 : L("“OpenLW In %@” becomes a stereo device.", "\(n)")
            confirmCut(message: coupled ? L("Uncouple this device?") : L("Couple this device?"), detail: detail, always: true) { [weak self] in
                self?.mutate(request)
            }
            return
        }
        // Coupling releases what does not fit a stereo patch of the first input's source.
        let chs = Set(groups[h].channels)
        let owners = config.inputs.filter { $0.taps.contains { chs.contains($0.channel) } }
        if !coupled && owners.count > 1, let window = window {
            let alert = NSAlert()
            alert.messageText = L("Couple inputs %@-%@?", "\(groups[h].channels.first ?? 1)", "\(groups[h].channels.last ?? 2)")
            alert.informativeText = L("The source of the first input becomes stereo on the pair; the other patches on these inputs are released.")
            alert.addButton(withTitle: L("Couple"))
            alert.addButton(withTitle: L("Cancel"))
            alert.beginSheetModal(for: window) { [weak self] response in
                if response == .alertFirstButtonReturn { self?.mutate(request) }
            }
            return
        }
        mutate(request)
    }

    /// Multi layout: send an input patch (`width` nil: release), warning first if it changes
    /// a device's width (the daemon then recreates the region).
    private func send(_ request: [String: Any], source s: DiscoveredSource, device: Int?, width: Int?) {
        guard config.multi, let n = device else {
            mutate(request)
            return
        }
        let before = DaemonConfig.inWidths(config.inputs, uncoupled: config.uncoupled, devices: config.inDevices)
        // After: the target device carries this source (or nothing); a surround makes it 8 wide.
        var after = config.inputs.map { p in
            InputPatch(channel: p.channel, group: p.group, kind: p.kind, taps: p.taps.filter { $0.device != n })
        }
        if let w = width, w == 8 {
            after.append(InputPatch(channel: s.channel, group: nil, kind: "surround",
                                    taps: (1...8).map { Tap(device: n, channel: $0, from: [$0]) }))
        }
        let widths = DaemonConfig.inWidths(after, uncoupled: config.uncoupled, devices: config.inDevices)
        let changed = zip(before, widths).enumerated().filter { $0.element.0 != $0.element.1 }
        guard let first = changed.first else {
            mutate(request)
            return
        }
        let (from, to) = first.element
        let what = to == 1 ? L("“OpenLW In %@” changes from %@ to 1 channel.", "\(first.offset + 1)", "\(from)")
            : L("“OpenLW In %@” changes from %@ to %@ channels.", "\(first.offset + 1)", "\(from)", "\(to)")
        confirmCut(message: width == nil ? L("Release this input?") : L("Patch this source?"), detail: what, always: true) { [weak self] in
            self?.mutate(request)
        }
    }

    /// Remove manual/unadvertised row; release inputs if patched.
    private func removeRow(_ row: Int) {
        guard row < gridRows.count else { return }
        let s = gridRows[row].source
        if let l = listening, l.channel == s.channel, l.kind == s.patchKind {
            stopListening()
        }
        manual.removeAll { $0.channel == s.channel && $0.patchKind == s.patchKind }
        // A received stream stays in the configuration, even unpatched (displaced by another
        // patch): without remove_input the row would come back as "not advertised".
        if config.inputs.contains(where: { $0.channel == s.channel && $0.kind == s.patchKind }) {
            mutate(["cmd": "remove_input", "channel": s.channel, "kind": s.patchKind])
        } else {
            updateGrid()
        }
    }

    @objc private func namingToggled(_ sender: NSButton) {
        mutate(["cmd": "set_device_naming", "enabled": sender.state == .on])
    }

    private static let manualKey = "manualSources"

    private static func loadManual() -> [DiscoveredSource] {
        let list = UserDefaults.standard.array(forKey: manualKey) as? [[String: Any]] ?? []
        return list.compactMap { d in
            guard let ch = d["channel"] as? Int else { return nil }
            return DiscoveredSource(channel: ch, name: "", kind: d["kind"] as? String ?? "stereo", terminal: "")
        }
    }

    private func saveManual() {
        UserDefaults.standard.set(manual.map { ["channel": $0.channel, "kind": $0.kind] }, forKey: Self.manualKey)
    }

    private func toggleListen(row: Int) {
        guard row < gridRows.count else { return }
        let s = gridRows[row].source
        if let l = listening, l.channel == s.channel, l.kind == s.patchKind {
            stopListening()
            return
        }
        listen(to: s)
    }

    /// Preview `s` on Mac's default audio output.
    func listen(to s: DiscoveredSource) {
        guard !statusIface.isEmpty, !statusIP.isEmpty else {
            show(.refused(L("the Mac is not connected to the Livewire network yet. Choose the interface, then try again.")))
            return
        }
        if MacAudio.livewireIsDefault(input: false) {
            show(.refused(L("the Mac's output is an OpenLW device, so listening would send the audio back to the network. Choose another output in System Settings > Sound.")))
            return
        }
        let group = s.stream.isEmpty ? livewireGroup(channel: s.channel, kind: s.patchKind) : s.stream
        do {
            try listener.start(group: group, iface: statusIface, ifaceIP: statusIP,
                               channels: s.patchKind == "surround" ? 8 : 2, bits: s.kind == "stereo-l16" ? 16 : 24)
            listening = (s.channel, s.patchKind)
            show(nil)
        } catch let e as ListenError {
            listening = nil
            messageLabel.stringValue = e.message
            messageLabel.isHidden = false
        } catch {
            listening = nil
        }
        updateGrid()
    }

    private func stopListening() {
        listener.stop()
        listening = nil
        updateGrid()
    }

    private func applyOutput(_ row: OutputRow) {
        let previous = outputPatch(row)
        guard row.enabled else {
            if let p = previous { mutate(["cmd": "unpatch_output", "channel": p.channel]) }
            return
        }
        guard let ch = row.channel else {
            show(.refused(L("invalid channel. Enter a number from 1 to 32766.")))
            row.show(previous)
            return
        }
        let send: () -> Void = { [weak self] in
            var req: [String: Any] = ["cmd": "patch_output", "channel": ch, "name": row.name, "format": row.format,
                                      "device_channels": row.pair]
            if let d = row.device { req["device"] = d }
            self?.mutate(req)
        }
        // Channel change: stop this row's old stream first.
        if let p = previous, p.channel != ch {
            client.call(["cmd": "unpatch_output", "channel": p.channel]) { [weak self] result in
                if case .failure(let e) = result { self?.show(e) } else { send() }
            }
        } else {
            send()
        }
    }
}

/// Flipped document view (content anchored at scroll top).
final class FlippedView: NSView {
    override var isFlipped: Bool { true }
}

/// Output row: Mac pair (duplex layout) or output device (multi layout), meter, channel, name,
/// format, transmission.
final class OutputRow: NSStackView, NSTextFieldDelegate {
    /// Device channels sent in the patch: the pair, or 1-2 of `device`.
    let pair: [Int]
    /// Multi layout: output device number.
    let device: Int?
    let meter = MeterView()
    private let channelField = NSTextField()
    private let nameField = NSTextField()
    private let formatPopup = NSPopUpButton()
    private let emitCheck = NSButton(checkboxWithTitle: L("Transmit"), target: nil, action: nil)
    var onApply: ((OutputRow) -> Void)?

    static let formats = [("standard", L("Standard (5 ms)")), ("aes67", L("AES67 (1 ms)")), ("livestream", L("Livestream (0.25 ms)"))]

    init(pair: [Int], device: Int? = nil) {
        self.pair = pair
        self.device = device
        super.init(frame: .zero)
        orientation = .horizontal
        spacing = 10
        let label = NSTextField(labelWithString: device.map { L("Out %@", "\($0)") } ?? L("Outputs %@-%@", "\(pair[0])", "\(pair[1])"))
        label.font = Theme.body
        label.widthAnchor.constraint(equalToConstant: 80).isActive = true
        meter.translatesAutoresizingMaskIntoConstraints = false
        meter.widthAnchor.constraint(equalToConstant: 110).isActive = true
        meter.heightAnchor.constraint(equalToConstant: 10).isActive = true
        meter.levels = [nil, nil]
        channelField.placeholderString = L("channel")
        channelField.font = Theme.mono
        channelField.widthAnchor.constraint(equalToConstant: 70).isActive = true
        nameField.placeholderString = L("advertised name")
        nameField.widthAnchor.constraint(equalToConstant: 170).isActive = true
        formatPopup.addItems(withTitles: Self.formats.map { $0.1 })
        for f in [channelField, nameField] {
            f.target = self
            f.action = #selector(commit(_:))
        }
        formatPopup.target = self
        formatPopup.action = #selector(commit(_:))
        emitCheck.target = self
        emitCheck.action = #selector(toggled(_:))
        [label, meter, channelField, nameField, formatPopup, emitCheck].forEach(addArrangedSubview)
        show(nil)
    }

    required init?(coder: NSCoder) { fatalError("unused") }

    var enabled: Bool { emitCheck.state == .on }
    var channel: Int? {
        guard let n = Int(channelField.stringValue.trimmingCharacters(in: .whitespaces)), (1...32766).contains(n) else { return nil }
        return n
    }
    var name: String {
        let n = nameField.stringValue.trimmingCharacters(in: .whitespaces)
        return n.isEmpty ? (device.map { "MAC \($0)" } ?? "MAC \(pair[0])-\(pair[1])") : n
    }
    var format: String { Self.formats[max(0, formatPopup.indexOfSelectedItem)].0 }

    private var editing: Bool {
        guard let fr = window?.firstResponder as? NSTextView else { return false }
        return fr.delegate === channelField || fr.delegate === nameField
    }

    /// Display configured state, except during editing.
    func show(_ patch: OutputPatch?) {
        guard !editing else { return }
        emitCheck.state = patch == nil ? .off : .on
        if let p = patch {
            channelField.stringValue = "\(p.channel)"
            nameField.stringValue = p.name
            if let idx = Self.formats.firstIndex(where: { $0.0 == p.format }) { formatPopup.selectItem(at: idx) }
        }
    }

    @objc private func commit(_ sender: Any) {
        if enabled { onApply?(self) }
    }

    @objc private func toggled(_ sender: Any) {
        onApply?(self)
    }
}
