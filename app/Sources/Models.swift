// Modèles décodés depuis les réponses JSON du daemon. Les crêtes « -inf » arrivent en null.

import Foundation

struct Iface {
    let name: String
    let friendly: String
    let ipv4: String
    let loopback: Bool
    /// Interface Ethernet susceptible de porter Livewire (hors bouclage et interfaces virtuelles).
    let candidate: Bool
    /// Annonces Livewire entendues récemment sur cette interface.
    let livewire: Bool

    init?(_ d: [String: Any]) {
        guard let name = d["name"] as? String, let ip = d["ipv4"] as? String else { return nil }
        self.name = name
        friendly = d["friendly"] as? String ?? name
        ipv4 = ip
        loopback = d["loopback"] as? Bool ?? false
        candidate = d["candidate"] as? Bool ?? !loopback
        livewire = d["livewire"] as? Bool ?? false
    }

    /// Libellé du menu : nom convivial, nom BSD, adresse.
    var title: String {
        let base = friendly == name ? "\(name) · \(ipv4)" : "\(friendly) (\(name)) · \(ipv4)"
        return livewire ? base + " · réseau Livewire" : base
    }
}

struct DiscoveredSource: Equatable {
    let channel: Int
    let name: String
    let stream: String
    let kind: String
    let terminal: String

    init?(_ d: [String: Any]) {
        guard let ch = d["channel"] as? Int else { return nil }
        channel = ch
        name = d["name"] as? String ?? ""
        stream = d["stream"] as? String ?? ""
        kind = d["kind"] as? String ?? "stereo"
        terminal = d["terminal"] as? String ?? ""
    }

    init(channel: Int, name: String, kind: String, terminal: String) {
        self.channel = channel
        self.name = name
        stream = ""
        self.kind = kind
        self.terminal = terminal
    }

    /// Valeur `kind` du daemon pour un patch d'entrée (les variantes stéréo annoncées donnent « stereo »).
    var patchKind: String { ["stereo", "backfeed", "surround"].contains(kind) ? kind : "stereo" }
}

/// Flux reçu de la configuration (destination).
struct InputPatch: Equatable {
    let channel: Int?
    let group: String?
    let kind: String
    let deviceChannels: [Int]
}

/// Flux émis de la configuration (source).
struct OutputPatch: Equatable {
    let channel: Int
    let name: String
    let format: String
    let deviceChannels: [Int]?
}

struct DaemonConfig {
    var iface = ""
    var advertise = true
    var inputs: [InputPatch] = []
    var outputs: [OutputPatch] = []
    var channelsToNet = 2
    var channelsFromNet = 2
    /// Nom du périphérique d'après les sources reçues.
    var nameFromSources = false
    /// Présentation dans macOS : "duplex" (un périphérique) ou "split" (In et Out).
    var layout = "duplex"
    /// Nom annoncé (vide : nom de l'ordinateur), préréglage de latence, octet TOS.
    var terminalName = ""
    var latency = "normal"
    var tos = 184

    /// Interface choisie automatiquement.
    var autoIface: Bool { iface.isEmpty || iface == "auto" }

    init() {}

    init(_ d: [String: Any]) {
        iface = d["iface"] as? String ?? ""
        advertise = d["advertise"] as? Bool ?? true
        nameFromSources = d["name_device_from_sources"] as? Bool ?? false
        layout = d["device_layout"] as? String ?? "duplex"
        terminalName = d["terminal_name"] as? String ?? ""
        latency = d["latency"] as? String ?? "normal"
        tos = d["tos"] as? Int ?? 184
        if let dev = d["device"] as? [String: Any] {
            channelsToNet = dev["channels_to_net"] as? Int ?? 2
            channelsFromNet = dev["channels_from_net"] as? Int ?? 2
        }
        inputs = (d["destinations"] as? [[String: Any]] ?? []).map {
            InputPatch(channel: $0["channel"] as? Int, group: $0["group"] as? String,
                       kind: $0["kind"] as? String ?? "stereo", deviceChannels: $0["device_channels"] as? [Int] ?? [])
        }
        outputs = (d["sources"] as? [[String: Any]] ?? []).compactMap {
            guard let ch = $0["channel"] as? Int else { return nil }
            return OutputPatch(channel: ch, name: $0["name"] as? String ?? "", format: $0["format"] as? String ?? "standard",
                               deviceChannels: $0["device_channels"] as? [Int])
        }
    }
}

struct DeviceMeters {
    var toNet: [Double?] = []
    var fromNet: [Double?] = []
    var inputsPrimed: [[Int]: Bool] = [:]
    var inputSlips: [[Int]: Int] = [:]

    init() {}

    init(_ status: [String: Any]) {
        guard let dev = status["device"] as? [String: Any] else { return }
        toNet = (dev["to_net_peak_dbfs"] as? [Any] ?? []).map { $0 as? Double }
        fromNet = (dev["from_net_peak_dbfs"] as? [Any] ?? []).map { $0 as? Double }
        for r in dev["inputs"] as? [[String: Any]] ?? [] {
            let chs = r["device_channels"] as? [Int] ?? []
            inputsPrimed[chs] = r["primed"] as? Bool ?? false
            inputSlips[chs] = (r["bus"] as? [String: Any])?["slips"] as? Int ?? 0
        }
    }

    /// Crête maximale (dBFS) d'un ensemble de canaux 1-based, ou nil si silence.
    static func peak(_ values: [Double?], channels: [Int]) -> Double? {
        channels.compactMap { c in c >= 1 && c <= values.count ? values[c - 1] : nil }.max()
    }
}

/// Liaison réseau : interface de la session, ou recherche.
struct LinkStatus {
    var auto = true
    var searching = true
    var iface = ""
    var friendly = ""
    var ipv4 = ""

    init() {}

    init(_ status: [String: Any]) {
        auto = status["iface_auto"] as? Bool ?? false
        iface = status["iface"] as? String ?? ""
        friendly = status["iface_friendly"] as? String ?? iface
        ipv4 = status["ipv4"] as? String ?? ""
        searching = status["searching"] as? Bool ?? iface.isEmpty
    }
}
