// Models decoded from daemon JSON responses. “-inf” peaks arrive as null.

import Foundation

struct Iface {
    let name: String
    let friendly: String
    let ipv4: String
    let loopback: Bool
    /// Ethernet interface potentially carrying Livewire (excluding loopback/virtual interfaces).
    let candidate: Bool
    /// Livewire advertisements recently heard on this interface.
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

    /// Menu label: friendly name, BSD name, address.
    var title: String {
        let base = friendly == name ? "\(name) · \(ipv4)" : "\(friendly) (\(name)) · \(ipv4)"
        return livewire ? base + L(" · Livewire network") : base
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

    /// Daemon `kind` value for input patch (advertised stereo variants yield “stereo”).
    var patchKind: String { ["stereo", "backfeed", "surround"].contains(kind) ? kind : "stereo" }
}

/// Configured received stream (destination).
struct InputPatch: Equatable {
    let channel: Int?
    let group: String?
    let kind: String
    /// Duplex layout: channels of the OpenLW device; multi layout: channels within `device`.
    let deviceChannels: [Int]
    /// Multi layout: input device number (“OpenLW In n”).
    let device: Int?
    /// Mono patch: "left", "right", or "sum".
    let mix: String?

    /// Device width this patch needs (multi layout).
    var width: Int { mix != nil ? 1 : (kind == "surround" ? 8 : 2) }
}

/// Configured transmitted stream (source).
struct OutputPatch: Equatable {
    let channel: Int
    let name: String
    let format: String
    let deviceChannels: [Int]?
    /// Multi layout: output device number (“OpenLW Out n”).
    let device: Int?
}

struct DaemonConfig {
    var iface = ""
    var advertise = true
    var inputs: [InputPatch] = []
    var outputs: [OutputPatch] = []
    var channelsToNet = 2
    var channelsFromNet = 2
    /// Multi layout: devices named after their source (default), otherwise “OpenLW In n”.
    var customNames = true
    /// macOS layout: "duplex" (one OpenLW device) or "multi" (OpenLW In n / OpenLW Out n).
    var layout = "duplex"
    /// Advertised name (empty: computer name), latency preset, TOS byte.
    var terminalName = ""
    var latency = "normal"
    var tos = 184

    /// Automatically selected interface.
    var autoIface: Bool { iface.isEmpty || iface == "auto" }

    var multi: Bool { layout == "multi" }

    /// Multi layout: device count per direction (one per configured pair).
    var inDevices: Int { (channelsFromNet + 1) / 2 }
    var outDevices: Int { (channelsToNet + 1) / 2 }

    /// Multi layout: width of each input device after `inputs` (2 when empty), as computed by
    /// the daemon (daemon/lw-daemon/src/config.rs, `in_widths`).
    static func inWidths(_ inputs: [InputPatch], devices: Int) -> [Int] {
        (1...max(1, devices)).map { n in
            inputs.first { $0.device == n && !$0.deviceChannels.isEmpty }?.width ?? 2
        }
    }

    init() {}

    init(_ d: [String: Any]) {
        iface = d["iface"] as? String ?? ""
        advertise = d["advertise"] as? Bool ?? true
        customNames = d["custom_device_names"] as? Bool ?? true
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
                       kind: $0["kind"] as? String ?? "stereo", deviceChannels: $0["device_channels"] as? [Int] ?? [],
                       device: $0["device"] as? Int, mix: $0["mix"] as? String)
        }
        outputs = (d["sources"] as? [[String: Any]] ?? []).compactMap {
            guard let ch = $0["channel"] as? Int else { return nil }
            return OutputPatch(channel: ch, name: $0["name"] as? String ?? "", format: $0["format"] as? String ?? "standard",
                               deviceChannels: $0["device_channels"] as? [Int], device: $0["device"] as? Int)
        }
    }
}

struct DeviceMeters {
    /// Peaks per channel, devices concatenated in order (one device in duplex layout).
    var toNet: [Double?] = []
    var fromNet: [Double?] = []
    /// Channels of each output / input device of the running region.
    var outWidths: [Int] = []
    var inWidths: [Int] = []
    /// Route state keyed by device channels (1-based, devices concatenated).
    var inputsPrimed: [[Int]: Bool] = [:]
    var inputSlips: [[Int]: Int] = [:]

    init() {}

    init(_ status: [String: Any]) {
        guard let dev = status["device"] as? [String: Any] else { return }
        toNet = (dev["to_net_peak_dbfs"] as? [Any] ?? []).map { $0 as? Double }
        fromNet = (dev["from_net_peak_dbfs"] as? [Any] ?? []).map { $0 as? Double }
        outWidths = dev["out_widths"] as? [Int] ?? []
        inWidths = dev["in_widths"] as? [Int] ?? []
        for r in dev["inputs"] as? [[String: Any]] ?? [] {
            let chs = r["device_channels"] as? [Int] ?? []
            inputsPrimed[chs] = r["primed"] as? Bool ?? false
            inputSlips[chs] = (r["bus"] as? [String: Any])?["slips"] as? Int ?? 0
        }
    }

    /// Concatenated 1-based channels of device `n` (1-based) given device widths.
    static func channels(device n: Int, widths: [Int]) -> [Int] {
        guard n >= 1, n <= widths.count else { return [] }
        let start = widths.prefix(n - 1).reduce(0, +)
        return Array((start + 1)...(start + widths[n - 1]))
    }

    /// Maximum peak (dBFS) across 1-based channels, or nil for silence.
    static func peak(_ values: [Double?], channels: [Int]) -> Double? {
        channels.compactMap { c in c >= 1 && c <= values.count ? values[c - 1] : nil }.max()
    }
}

/// Network connection: session interface or searching.
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
