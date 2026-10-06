// Fenêtre principale : liaison au réseau Livewire, grille de patch des entrées, sorties diffusées.
// Le service réseau (LaunchDaemon) n'apparaît pas : l'app parle de réseau, de canaux et du périphérique.
// Relevé de l'état à 5 Hz ; sources, configuration, interfaces et périphériques du Mac toutes les 2 s.

import AppKit

final class MainWindowController: NSWindowController, NSTextFieldDelegate, NSWindowDelegate {
    private let client: DaemonClient

    // Dernier état connu.
    private var config = DaemonConfig()
    private var configLoaded = false
    private var meters = DeviceMeters()
    private var discovered: [DiscoveredSource] = []
    private var ifaces: [Iface] = []
    /// Canaux saisis à la main, mémorisés entre deux lancements.
    private var manual: [DiscoveredSource] = MainWindowController.loadManual() {
        didSet { saveManual() }
    }
    private var reachable = false
    private var link = LinkStatus()
    /// Interface de la session en cours (pour la pré-écoute).
    private var statusIface: String { link.searching ? "" : link.iface }
    private var statusIP: String { link.searching ? "" : link.ipv4 }

    // Pré-écoute : une source à la fois, repérée par canal et type (l'ordre des lignes change).
    private let listener = Listener()
    private var listening: (channel: Int, kind: String)?
    private var busy: Set<String> = []

    // En-tête.
    private let stateDot = NSView()
    private let stateLabel = NSTextField(labelWithString: "Connexion au service OpenLW…")
    private let ifacePopup = NSPopUpButton()
    private let advertiseCheck = NSButton(checkboxWithTitle: "Annoncer les sorties sur le réseau", target: nil, action: nil)
    private let messageLabel = NSTextField(wrappingLabelWithString: "")

    // Nombre de canaux Livewire dans chaque sens (paires du périphérique, 1 à 16).
    static let maxPairs = 16
    private let inCount = NSPopUpButton()
    private let outCount = NSPopUpButton()

    // Entrée et sortie par défaut du Mac.
    private let macInputLabel = NSTextField(labelWithString: "")
    private let macOutputLabel = NSTextField(labelWithString: "")
    private let useInput = NSButton(title: "Utiliser OpenLW", target: nil, action: nil)
    private let useOutput = NSButton(title: "Utiliser OpenLW", target: nil, action: nil)

    // Entrées.
    private let namingCheck = NSButton(checkboxWithTitle: "Nommer les périphériques d'après les canaux patchés, par exemple « OpenLW In (2 - Studio A) »",
                                       target: nil, action: nil)
    private let layoutPopup = NSPopUpButton()
    static let layouts = [("duplex", "Un périphérique « OpenLW » (entrée et sortie)"),
                          ("split", "Deux périphériques « OpenLW In » et « OpenLW Out »")]
    private let grid = InputGridView()
    private let manualChannel = NSTextField()
    private let manualKind = NSPopUpButton()

    // Sorties.
    private let outputStack = NSStackView()
    private var outputRows: [OutputRow] = []

    // Réglages avancés.
    private let advancedToggle = NSButton()
    private let advancedBody = NSStackView()
    private let terminalField = NSTextField()
    private let latencyPopup = NSPopUpButton()
    private let dscpPopup = NSPopUpButton()
    static let latencies = [("low", "Faible : ≈ 8 ms ajoutées, réseau dédié"),
                            ("normal", "Normale : ≈ 17 ms ajoutées"),
                            ("safe", "Sûre : ≈ 35 ms ajoutées, réseau partagé ou Mac chargé")]
    static let dscps = [(46, "EF (46) : défaut Livewire"), (34, "AF41 (34) : recommandé pour AES67"), (0, "Aucune (0)")]
    private static let advancedKey = "advancedVisible"

    private var timers: [Timer] = []

    init(window: NSWindow, client: DaemonClient) {
        self.client = client
        super.init(window: window)
    }

    required init?(coder: NSCoder) { fatalError("non utilisé") }

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
    }

    private func buildHeader() -> GlassPanel {
        let panel = GlassPanel(title: "Réseau Livewire")
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
        let ifaceLabel = NSTextField(labelWithString: "Interface :")
        ifaceLabel.font = Theme.body
        let controls = NSStackView(views: [ifaceLabel, ifacePopup, NSView(), advertiseCheck])
        controls.spacing = 8
        ifacePopup.widthAnchor.constraint(greaterThanOrEqualToConstant: 280).isActive = true

        let clock = NSTextField(labelWithString: "Horloge : celle du Mac. Les écarts avec les autres appareils sont compensés automatiquement.")
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
        let panel = GlassPanel(title: "Entrées du Mac (réseau vers Mac)")
        let hint = NSTextField(wrappingLabelWithString: "Cliquez une case pour envoyer la source sur cette paire d'entrées du périphérique OpenLW. Cliquez à nouveau pour la libérer. Les applications enregistrent depuis « OpenLW » (ou « OpenLW In »). Le bouton casque fait écouter la source sur la sortie du Mac, sans la patcher.")
        hint.font = Theme.small
        hint.textColor = .secondaryLabelColor

        grid.translatesAutoresizingMaskIntoConstraints = false
        grid.onToggle = { [weak self] row, col in self?.toggleInput(row: row, column: col) }
        grid.onListen = { [weak self] row in self?.toggleListen(row: row) }
        grid.onRemove = { [weak self] row in self?.removeRow(row) }


        let addLabel = NSTextField(labelWithString: "Source non annoncée, canal :")
        addLabel.font = Theme.body
        manualChannel.placeholderString = "1 à 32766"
        manualChannel.font = Theme.mono
        manualChannel.target = self
        manualChannel.action = #selector(addManual(_:))
        manualChannel.widthAnchor.constraint(equalToConstant: 90).isActive = true
        manualKind.addItems(withTitles: ["Stéréo", "Retour (To Source)", "Surround 8 canaux"])
        let add = NSButton(title: "Ajouter à la grille", target: self, action: #selector(addManual(_:)))
        let addRow = NSStackView(views: [addLabel, manualChannel, manualKind, add])
        addRow.spacing = 8

        let countRow = countControls(inCount, label: "Canaux Livewire reçus :", unit: "entrées",
                                     macLabel: macInputLabel, use: useInput, useAction: #selector(useOpenLWInput(_:)))
        let v = NSStackView(views: [countRow, hint, grid, addRow])
        v.orientation = .vertical
        v.alignment = .leading
        v.spacing = 10
        embed(v, in: panel.content)
        hint.widthAnchor.constraint(equalTo: v.widthAnchor).isActive = true
        countRow.widthAnchor.constraint(equalTo: v.widthAnchor).isActive = true
        return panel
    }

    private func buildDevice() -> GlassPanel {
        let panel = GlassPanel(title: "Périphérique audio")
        let label = NSTextField(labelWithString: "Présentation dans macOS :")
        label.font = Theme.body
        layoutPopup.addItems(withTitles: Self.layouts.map { $0.1 })
        layoutPopup.target = self
        layoutPopup.action = #selector(layoutChosen(_:))
        let row = NSStackView(views: [label, layoutPopup])
        row.spacing = 8
        namingCheck.target = self
        namingCheck.action = #selector(namingToggled(_:))
        let hint = NSTextField(wrappingLabelWithString: "Avec deux périphériques, l'entrée et la sortie portent chacune leur nom dans les applications. Les canaux portent toujours le nom de leur source (Configuration audio et MIDI, Logic…). Après un changement de présentation ou de nom, sélectionnez de nouveau le périphérique dans les applications qui le retrouvent par son nom, comme Audacity.")
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
        guard value != config.layout else { return }
        mutate(["cmd": "set_device_layout", "layout": value])
    }

    private func buildOutputs() -> GlassPanel {
        let panel = GlassPanel(title: "Sorties du Mac (Mac vers réseau)")
        let hint = NSTextField(wrappingLabelWithString: "Choisissez « OpenLW » (ou « OpenLW Out ») comme sortie du Mac ou de votre application. Chaque paire de sorties est diffusée sur le canal Livewire de votre choix, sous le nom indiqué. Une modification s'applique quand la case Diffuser est cochée.")
        hint.font = Theme.small
        hint.textColor = .secondaryLabelColor
        outputStack.orientation = .vertical
        outputStack.alignment = .leading
        outputStack.spacing = 6
        let countRow = countControls(outCount, label: "Canaux Livewire diffusés :", unit: "sorties",
                                     macLabel: macOutputLabel, use: useOutput, useAction: #selector(useOpenLWOutput(_:)))
        let v = NSStackView(views: [countRow, hint, outputStack])
        v.orientation = .vertical
        v.alignment = .leading
        v.spacing = 10
        embed(v, in: panel.content)
        hint.widthAnchor.constraint(equalTo: v.widthAnchor).isActive = true
        countRow.widthAnchor.constraint(equalTo: v.widthAnchor).isActive = true
        rebuildOutputRows(pairs: 1)
        return panel
    }

    private func buildAdvanced() -> GlassPanel {
        let panel = GlassPanel(title: "Réglages avancés")
        advancedToggle.setButtonType(.pushOnPushOff)
        advancedToggle.bezelStyle = .disclosure
        advancedToggle.title = ""
        advancedToggle.target = self
        advancedToggle.action = #selector(advancedToggled(_:))
        let toggleLabel = NSTextField(labelWithString: "Nom annoncé, latence de réception, priorité réseau")
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
        terminalField.placeholderString = Host.current().localizedName ?? "nom de l'ordinateur"
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
            row("Nom annoncé du Mac :", terminalField,
                "Nom affiché par les autres appareils Livewire, 32 caractères au plus, accents remplacés. Vide : nom de l'ordinateur. Validez avec Retour."),
            row("Latence de réception :", latencyPopup,
                "Tampons ajoutés à celui de l'application qui enregistre. Plus la latence est faible, plus un retard du réseau ou du Mac risque de provoquer une coupure brève."),
            row("Priorité réseau (DSCP) :", dscpPopup,
                "Marquage des flux audio émis par le Mac. Choisissez la valeur prévue par la QoS de vos commutateurs."),
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

    /// Ligne « nombre de canaux » d'un panneau, avec le périphérique par défaut du Mac dans ce sens.
    private func countControls(_ popup: NSPopUpButton, label: String, unit: String, macLabel: NSTextField,
                               use: NSButton, useAction: Selector) -> NSStackView {
        let title = NSTextField(labelWithString: label)
        title.font = Theme.body
        popup.addItems(withTitles: (1...Self.maxPairs).map { n in
            "\(n) \(n == 1 ? "canal" : "canaux") (\(2 * n) \(unit))"
        })
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

    private func rebuildOutputRows(pairs: Int) {
        outputRows.forEach { $0.removeFromSuperview() }
        outputRows = (0..<pairs).map { i in
            let row = OutputRow(pair: [2 * i + 1, 2 * i + 2])
            row.onApply = { [weak self] r in self?.applyOutput(r) }
            return row
        }
        outputRows.forEach(outputStack.addArrangedSubview)
    }

    // MARK: - Relevés

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

    /// Première activation : pas de champ actif (macOS ferait défiler jusqu'au premier champ de saisie,
    /// le nom annoncé, en bas de la fenêtre) ; affichage depuis le haut.
    func windowDidBecomeKey(_ notification: Notification) {
        guard !shownOnce else { return }
        shownOnce = true
        window?.makeFirstResponder(nil)
        (window?.contentView?.subviews.first as? NSScrollView)?.documentView?.scroll(.zero)
    }

    /// Envoie `request` sauf si une requête de même nature est déjà en cours.
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
                break // commande absente d'un daemon plus ancien : l'information reste vide
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
                self.stopListening() // l'interface a changé : l'abonnement n'est plus valable
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

    /// Ligne d'état : interface utilisée, ou recherche du réseau.
    private func showLink() {
        if link.searching {
            stateDot.layer?.backgroundColor = NSColor.systemOrange.cgColor
            stateLabel.stringValue = link.auto
                ? "Recherche du réseau Livewire. Branchez le Mac sur le réseau Livewire, ou choisissez l'interface."
                : "Interface « \(config.iface) » indisponible. Branchez-la, ou choisissez Automatique."
            return
        }
        stateDot.layer?.backgroundColor = NSColor.systemGreen.cgColor
        let name = link.friendly == link.iface ? link.iface : "\(link.friendly) (\(link.iface))"
        stateLabel.stringValue = "Connecté au réseau Livewire par \(name) · \(link.ipv4)"
            + (link.auto ? " · interface choisie automatiquement" : "")
    }

    /// Entrée et sortie par défaut du Mac, et boutons pour y mettre « OpenLW ».
    private func updateMacDevices() {
        for (input, label, button) in [(true, macInputLabel, useInput), (false, macOutputLabel, useOutput)] {
            let lw = MacAudio.livewireDevice(input: input)
            let current = MacAudio.defaultDevice(input: input)
            let name = current.map(MacAudio.name) ?? "aucune"
            label.stringValue = (input ? "Entrée du Mac : " : "Sortie du Mac : ") + name
            button.isHidden = lw == nil || current == lw
        }
    }

    // MARK: - Mise à jour de l'affichage

    private func applyConfig(_ c: DaemonConfig) {
        let pairsChanged = !configLoaded || c.channelsToNet != config.channelsToNet
        config = c
        configLoaded = true
        advertiseCheck.state = c.advertise ? .on : .off
        namingCheck.state = c.nameFromSources ? .on : .off
        namingCheck.isEnabled = c.layout == "split"
        if let i = Self.layouts.firstIndex(where: { $0.0 == c.layout }) { layoutPopup.selectItem(at: i) }
        let editingName = (window?.firstResponder as? NSTextView)?.delegate === terminalField
        if !editingName { terminalField.stringValue = c.terminalName }
        if let i = Self.latencies.firstIndex(where: { $0.0 == c.latency }) { latencyPopup.selectItem(at: i) }
        if let i = Self.dscps.firstIndex(where: { $0.0 << 2 == c.tos }) {
            dscpPopup.selectItem(at: i)
        } else {
            dscpPopup.select(nil) // valeur hors liste (config modifiée à la main)
        }
        if pairsChanged {
            rebuildOutputRows(pairs: max(1, c.channelsToNet / 2))
        }
        grid.pairs = stride(from: 1, through: c.channelsFromNet - 1, by: 2).map { [$0, $0 + 1] }
        inCount.selectItem(at: min(Self.maxPairs, max(1, c.channelsFromNet / 2)) - 1)
        outCount.selectItem(at: min(Self.maxPairs, max(1, c.channelsToNet / 2)) - 1)
        for row in outputRows {
            row.show(c.outputs.first { $0.deviceChannels == row.pair })
        }
        updateIfacePopup()
        updateGrid()
    }

    /// Menu : « Automatique », puis les interfaces Ethernet (et l'interface configurée si elle n'en est pas).
    private func updateIfacePopup() {
        let auto = config.autoIface
        let autoTitle: String
        if !auto {
            autoTitle = "Automatique"
        } else if link.searching {
            autoTitle = "Automatique · recherche en cours"
        } else {
            autoTitle = "Automatique · \(link.friendly)"
        }
        var items: [(String, String)] = [(autoTitle, "auto")]
        var listed = ifaces.filter { $0.candidate || (!auto && $0.name == config.iface) }
        listed.sort { ($0.livewire ? 0 : 1, $0.name) < ($1.livewire ? 0 : 1, $1.name) }
        items += listed.map { ($0.title, $0.name) }
        if !auto && !ifaces.contains(where: { $0.name == config.iface }) {
            items.append(("\(config.iface) (indisponible)", config.iface))
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

    /// Lignes de la grille : sources découvertes, puis flux configurés non annoncés, puis saisies.
    private var gridRows: [GridRow] = []

    private func patchedColumn(channel: Int, kind: String) -> Int? {
        guard let p = config.inputs.first(where: { $0.channel == channel && $0.kind == kind && !$0.deviceChannels.isEmpty }),
              let first = p.deviceChannels.first else { return nil }
        return (first - 1) / 2
    }

    private func updateGrid() {
        var rows: [GridRow] = []
        var seen = Set<String>()
        func add(_ s: DiscoveredSource, origin: String, removable: Bool) {
            let key = "\(s.channel)/\(s.patchKind)"
            guard !seen.contains(key) else { return }
            seen.insert(key)
            rows.append(GridRow(source: s, patchedColumn: patchedColumn(channel: s.channel, kind: s.patchKind),
                                origin: origin, removable: removable))
        }
        for s in discovered { add(s, origin: s.terminal, removable: false) }
        for s in manual { add(s, origin: "saisi", removable: true) }
        for p in config.inputs {
            guard let ch = p.channel else { continue }
            add(DiscoveredSource(channel: ch, name: "", kind: p.kind, terminal: ""), origin: "non annoncé", removable: true)
        }
        gridRows = rows
        grid.rows = rows
        grid.listeningRow = listening.flatMap { l in rows.firstIndex { $0.source.channel == l.channel && $0.source.patchKind == l.kind } }
        updateMeters()
    }

    private func updateMeters() {
        grid.listenLevel = listening == nil ? nil : listener.takePeak()
        grid.columnLevels = grid.pairs.map { pair in pair.map { DeviceMeters.peak(meters.fromNet, channels: [$0]) } }
        grid.columnStatus = grid.pairs.map { pair in
            guard let primed = meters.inputsPrimed.first(where: { $0.key.contains(pair[0]) })?.value else { return "libre" }
            return primed ? "audio reçu" : "en attente"
        }
        for row in outputRows {
            row.meter.levels = row.pair.map { DeviceMeters.peak(meters.toNet, channels: [$0]) }
        }
    }

    private func show(_ error: DaemonError?) {
        messageLabel.stringValue = error?.message ?? ""
        messageLabel.isHidden = error == nil
    }

    // MARK: - Actions

    /// Requête de modification ; la configuration renvoyée remplace l'état affiché.
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
        let lostOut = config.outputs.filter { ($0.deviceChannels?.max() ?? 0) > toNet }.count
        let lostIn = config.inputs.filter { ($0.deviceChannels.max() ?? 0) > fromNet }.count
        if lostOut + lostIn > 0, let window = window {
            let alert = NSAlert()
            alert.messageText = "Réduire le nombre de canaux ?"
            var lost: [String] = []
            if lostOut > 0 { lost.append("\(lostOut) diffusion\(lostOut > 1 ? "s" : "") arrêtée\(lostOut > 1 ? "s" : "")") }
            if lostIn > 0 { lost.append("\(lostIn) source\(lostIn > 1 ? "s" : "") retirée\(lostIn > 1 ? "s" : "") des entrées") }
            alert.informativeText = lost.joined(separator: ", ") + ". Le son du périphérique « OpenLW » s'interrompt un instant."
            alert.addButton(withTitle: "Réduire")
            alert.addButton(withTitle: "Annuler")
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
        mutate(["cmd": "set_device_channels", "to_net": toNet, "from_net": fromNet])
    }

    @objc private func useOpenLWInput(_ sender: Any) { useOpenLW(input: true) }
    @objc private func useOpenLWOutput(_ sender: Any) { useOpenLW(input: false) }

    private func useOpenLW(input: Bool) {
        guard let lw = MacAudio.livewireDevice(input: input) else {
            show(.refused("le périphérique OpenLW est introuvable. Réinstallez OpenLW."))
            return
        }
        if !MacAudio.setDefault(lw, input: input) {
            show(.refused("macOS a refusé de changer \(input ? "l'entrée" : "la sortie") par défaut."))
        }
        updateMacDevices()
    }

    @objc private func advertiseToggled(_ sender: NSButton) {
        mutate(["cmd": "set_advertise", "advertise": sender.state == .on])
    }

    @objc private func addManual(_ sender: Any) {
        guard let ch = Int(manualChannel.stringValue.trimmingCharacters(in: .whitespaces)), (1...32766).contains(ch) else {
            show(.refused("canal invalide. Saisissez un nombre de 1 à 32766."))
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

    private func toggleInput(row: Int, column: Int) {
        guard row < gridRows.count, column < grid.pairs.count else { return }
        let r = gridRows[row]
        let pair = grid.pairs[column]
        if r.patchedColumn == column {
            mutate(["cmd": "unpatch_input", "device_channels": pair])
            return
        }
        let width = r.source.patchKind == "surround" ? 8 : 2
        let first = pair[0]
        guard first + width - 1 <= config.channelsFromNet else {
            show(.refused("une source surround occupe 8 entrées. Choisissez une paire de 1-2 à \(config.channelsFromNet - 7)-\(config.channelsFromNet - 6)."))
            return
        }
        mutate(["cmd": "patch_input", "channel": r.source.channel, "kind": r.source.patchKind,
                "device_channels": Array(first..<(first + width))])
    }

    /// Retire une ligne saisie ou non annoncée ; libère ses entrées si elle est patchée.
    private func removeRow(_ row: Int) {
        guard row < gridRows.count else { return }
        let s = gridRows[row].source
        if let l = listening, l.channel == s.channel, l.kind == s.patchKind {
            stopListening()
        }
        manual.removeAll { $0.channel == s.channel && $0.patchKind == s.patchKind }
        if let p = config.inputs.first(where: { $0.channel == s.channel && $0.kind == s.patchKind }) {
            if p.deviceChannels.isEmpty {
                updateGrid() // flux reçu sans patch : disparaîtra au prochain relevé
            } else {
                mutate(["cmd": "unpatch_input", "device_channels": p.deviceChannels])
            }
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

    /// Écoute `s` sur la sortie audio par défaut du Mac.
    func listen(to s: DiscoveredSource) {
        guard !statusIface.isEmpty, !statusIP.isEmpty else {
            show(.refused("le Mac n'est pas encore relié au réseau Livewire. Choisissez l'interface, puis réessayez."))
            return
        }
        if MacAudio.livewireIsDefault(input: false) {
            show(.refused("la sortie du Mac est le périphérique OpenLW : l'écoute repartirait sur le réseau. Choisissez une autre sortie dans Réglages Système > Son."))
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
        let previous = config.outputs.first { $0.deviceChannels == row.pair }
        guard row.enabled else {
            if let p = previous { mutate(["cmd": "unpatch_output", "channel": p.channel]) }
            return
        }
        guard let ch = row.channel else {
            show(.refused("canal invalide. Saisissez un nombre de 1 à 32766."))
            row.show(previous)
            return
        }
        let send: () -> Void = { [weak self] in
            self?.mutate(["cmd": "patch_output", "channel": ch, "name": row.name, "format": row.format,
                          "device_channels": row.pair])
        }
        // Changement de canal : arrêter d'abord l'ancien flux de cette paire.
        if let p = previous, p.channel != ch {
            client.call(["cmd": "unpatch_output", "channel": p.channel]) { [weak self] result in
                if case .failure(let e) = result { self?.show(e) } else { send() }
            }
        } else {
            send()
        }
    }
}

/// Vue de document retournée (contenu ancré en haut du défilement).
final class FlippedView: NSView {
    override var isFlipped: Bool { true }
}

/// Ligne de sortie : paire du Mac, vumètre, canal, nom, format, diffusion.
final class OutputRow: NSStackView, NSTextFieldDelegate {
    let pair: [Int]
    let meter = MeterView()
    private let channelField = NSTextField()
    private let nameField = NSTextField()
    private let formatPopup = NSPopUpButton()
    private let emitCheck = NSButton(checkboxWithTitle: "Diffuser", target: nil, action: nil)
    var onApply: ((OutputRow) -> Void)?

    static let formats = [("standard", "Standard (5 ms)"), ("aes67", "AES67 (1 ms)"), ("livestream", "Livestream (0,25 ms)")]

    init(pair: [Int]) {
        self.pair = pair
        super.init(frame: .zero)
        orientation = .horizontal
        spacing = 10
        let label = NSTextField(labelWithString: "Sorties \(pair[0])-\(pair[1])")
        label.font = Theme.body
        label.widthAnchor.constraint(equalToConstant: 80).isActive = true
        meter.translatesAutoresizingMaskIntoConstraints = false
        meter.widthAnchor.constraint(equalToConstant: 110).isActive = true
        meter.heightAnchor.constraint(equalToConstant: 10).isActive = true
        meter.levels = [nil, nil]
        channelField.placeholderString = "canal"
        channelField.font = Theme.mono
        channelField.widthAnchor.constraint(equalToConstant: 70).isActive = true
        nameField.placeholderString = "nom annoncé"
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

    required init?(coder: NSCoder) { fatalError("non utilisé") }

    var enabled: Bool { emitCheck.state == .on }
    var channel: Int? {
        guard let n = Int(channelField.stringValue.trimmingCharacters(in: .whitespaces)), (1...32766).contains(n) else { return nil }
        return n
    }
    var name: String {
        let n = nameField.stringValue.trimmingCharacters(in: .whitespaces)
        return n.isEmpty ? "MAC \(pair[0])-\(pair[1])" : n
    }
    var format: String { Self.formats[max(0, formatPopup.indexOfSelectedItem)].0 }

    private var editing: Bool {
        guard let fr = window?.firstResponder as? NSTextView else { return false }
        return fr.delegate === channelField || fr.delegate === nameField
    }

    /// Affiche l'état configuré, sauf pendant une saisie.
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
