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
        return livewire ? base + " · " + L("Livewire network") : base
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

/// Crosspoint: one device input fed by stream channels (1-based: [1] left, [2] right,
/// [1, 2] L+R, [k] surround channel k).
struct Tap: Equatable {
    /// Multi layout: input device number (“OpenLW In n”).
    let device: Int?
    /// Device input (within `device` in multi layout).
    let channel: Int
    let from: [Int]

    init(device: Int?, channel: Int, from: [Int]) {
        self.device = device
        self.channel = channel
        self.from = from
    }

    init?(_ d: [String: Any]) {
        guard let ch = d["channel"] as? Int else { return nil }
        device = d["device"] as? Int
        channel = ch
        from = d["from"] as? [Int] ?? []
    }

    var json: [String: Any] {
        var d: [String: Any] = ["channel": channel, "from": from]
        if let n = device { d["device"] = n }
        return d
    }
}

/// Configured received stream (destination).
struct InputPatch: Equatable {
    let channel: Int?
    let group: String?
    let kind: String
    let taps: [Tap]
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

    /// Input pairs (duplex) or input devices (multi) not coupled in stereo.
    var uncoupled: Set<Int> = []

    func coupled(_ n: Int) -> Bool { !uncoupled.contains(n) }

    /// Multi layout: width of each input device, as computed by the daemon
    /// (daemon/lw-daemon/src/config.rs, `in_widths`): 1 uncoupled, 8 surround, otherwise 2.
    static func inWidths(_ inputs: [InputPatch], uncoupled: Set<Int>, devices: Int) -> [Int] {
        (1...max(1, devices)).map { n in
            if uncoupled.contains(n) { return 1 }
            return inputs.contains { $0.kind == "surround" && $0.taps.contains { $0.device == n } } ? 8 : 2
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
                       kind: $0["kind"] as? String ?? "stereo",
                       taps: ($0["taps"] as? [[String: Any]] ?? []).compactMap(Tap.init))
        }
        uncoupled = Set(d["uncoupled_inputs"] as? [Int] ?? [])
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
        setPeaks(dev)
        outWidths = dev["out_widths"] as? [Int] ?? []
        inWidths = dev["in_widths"] as? [Int] ?? []
        for r in dev["inputs"] as? [[String: Any]] ?? [] {
            let chs = r["device_channels"] as? [Int] ?? []
            inputsPrimed[chs] = r["primed"] as? Bool ?? false
            inputSlips[chs] = (r["bus"] as? [String: Any])?["slips"] as? Int ?? 0
        }
    }

    /// Peaks from a `status` device object or a `meters` reply.
    mutating func setPeaks(_ dev: [String: Any]) {
        toNet = (dev["to_net_peak_dbfs"] as? [Any] ?? []).map { $0 as? Double }
        fromNet = (dev["from_net_peak_dbfs"] as? [Any] ?? []).map { $0 as? Double }
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

/// Peak-meter ballistics, per meter and per bar: instant rise, then a fall of 20 dB in 1.7 s
/// (IEC 60268-10 type I return), so that short dips do not blank the bar.
struct MeterBallistics {
    static let floor = -60.0
    static let fallPerSecond = 20 / 1.7
    private(set) var shown: [[Double?]] = []

    /// All bars at the floor.
    var idle: Bool { shown.allSatisfy { $0.allSatisfy { $0 == nil } } }

    /// Advance `dt` seconds toward `targets` (dBFS, nil for silence).
    mutating func step(_ targets: [[Double?]], dt: Double) {
        shown = targets.enumerated().map { m, bars in
            bars.enumerated().map { b, target -> Double? in
                let held = m < shown.count && b < shown[m].count ? shown[m][b] : nil
                let level = [target, held.map { $0 - Self.fallPerSecond * dt }].compactMap { $0 }.max()
                return level.flatMap { $0 > Self.floor ? $0 : nil }
            }
        }
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
