// Models decoded from daemon JSON responses (same as macos/app/Sources/Models.swift).
// “-inf” peaks arrive as null.
using System.Text.Json.Nodes;

namespace OpenLW.Daemon;

internal static class Json
{
    public static string Str(JsonNode? n, string key, string fallback = "") =>
        n?[key] is JsonValue v && v.TryGetValue(out string? s) && s is not null ? s : fallback;

    public static int Int(JsonNode? n, string key, int fallback = 0) =>
        n?[key] is JsonValue v && v.TryGetValue(out int i) ? i : fallback;

    public static string? StrOrNull(JsonNode? n, string key) =>
        n?[key] is JsonValue v && v.TryGetValue(out string? s) ? s : null;

    public static int? IntOrNull(JsonNode? n, string key) =>
        n?[key] is JsonValue v && v.TryGetValue(out int i) ? i : null;

    public static bool Bool(JsonNode? n, string key, bool fallback = false) =>
        n?[key] is JsonValue v && v.TryGetValue(out bool b) ? b : fallback;

    public static double? Double(JsonNode? n) =>
        n is JsonValue v && v.TryGetValue(out double d) ? d : null;

    public static List<int> Ints(JsonNode? n, string key) =>
        n?[key] is JsonArray a ? a.Select(x => x is JsonValue v && v.TryGetValue(out int i) ? i : 0).ToList() : [];

    public static IEnumerable<JsonNode> Objects(JsonNode? n, string key) =>
        n?[key] is JsonArray a ? a.Where(x => x is JsonObject).Select(x => x!) : [];
}

public sealed record Iface(string Name, string Friendly, string Ipv4, bool Loopback, bool Candidate, bool Livewire)
{
    public static Iface? From(JsonNode n)
    {
        string name = Json.Str(n, "name"), ip = Json.Str(n, "ipv4");
        if (name.Length == 0 || ip.Length == 0)
        {
            return null;
        }
        bool loopback = Json.Bool(n, "loopback");
        return new Iface(name, Json.Str(n, "friendly", name), ip, loopback, Json.Bool(n, "candidate", !loopback),
            Json.Bool(n, "livewire"));
    }

    /// Menu label: friendly name, address.
    public string Title
    {
        get
        {
            string b = Friendly == Name ? $"{Name} · {Ipv4}" : $"{Friendly} ({Name}) · {Ipv4}";
            return Livewire ? $"{b} · {Loc.S("LivewireNetworkTag")}" : b;
        }
    }
}

public sealed record DiscoveredSource(int Channel, string Name, string Stream, string Kind, string Terminal)
{
    public static DiscoveredSource? From(JsonNode n)
    {
        int? ch = Json.IntOrNull(n, "channel");
        return ch is null ? null : new DiscoveredSource(ch.Value, Json.Str(n, "name"), Json.Str(n, "stream"),
            Json.Str(n, "kind", "stereo"), Json.Str(n, "terminal"));
    }

    /// Daemon `kind` value for the input patch (advertised stereo variants yield “stereo”).
    public string PatchKind => Kind is "stereo" or "backfeed" or "surround" ? Kind : "stereo";
}

/// Crosspoint: one driver input (1-based `Channel`) fed by stream channels (1-based `From`:
/// [1] left, [2] right, [1, 2] L+R, [k] surround channel k; empty in a request: release).
public sealed record Tap(int Channel, List<int> From)
{
    public static Tap? Parse(JsonNode n) =>
        Json.IntOrNull(n, "channel") is int ch ? new Tap(ch, Json.Ints(n, "from")) : null;

    public JsonObject ToJson() => new()
    {
        ["channel"] = Channel,
        ["from"] = new JsonArray(From.Select(f => (JsonNode?)JsonValue.Create(f)).ToArray()),
    };
}

/// Configured received stream (destination) and its crosspoints (none: received for
/// statistics only).
public sealed record InputPatch(int? Channel, string? Group, string Kind, List<Tap> Taps);

/// Configured transmitted stream (source).
public sealed record OutputPatch(int Channel, string Name, string Format, List<int>? DeviceChannels);

public sealed class DaemonConfig
{
    public string Iface { get; init; } = "";
    public bool Advertise { get; init; } = true;
    public List<InputPatch> Inputs { get; init; } = [];
    public List<OutputPatch> Outputs { get; init; } = [];
    public int ChannelsToNet { get; init; } = 2;
    public int ChannelsFromNet { get; init; } = 2;
    public string TerminalName { get; init; } = "";
    public string Latency { get; init; } = "normal";
    public int Tos { get; init; } = 184;
    /// Input pairs (pair n = inputs 2n − 1 and 2n) not coupled in stereo.
    public HashSet<int> Uncoupled { get; init; } = [];

    /// Pair `n` (1-based) coupled in stereo (the default).
    public bool Coupled(int n) => !Uncoupled.Contains(n);

    /// Automatically selected interface.
    public bool AutoIface => Iface.Length == 0 || Iface == "auto";

    public static DaemonConfig From(JsonNode c)
    {
        JsonNode? dev = c["device"];
        return new DaemonConfig
        {
            Iface = Json.Str(c, "iface"),
            Advertise = Json.Bool(c, "advertise", true),
            TerminalName = Json.Str(c, "terminal_name"),
            Latency = Json.Str(c, "latency", "normal"),
            Tos = Json.Int(c, "tos", 184),
            ChannelsToNet = Json.Int(dev, "channels_to_net", 2),
            ChannelsFromNet = Json.Int(dev, "channels_from_net", 2),
            Inputs = Json.Objects(c, "destinations").Select(d => new InputPatch(Json.IntOrNull(d, "channel"),
                Json.StrOrNull(d, "group"), Json.Str(d, "kind", "stereo"),
                Json.Objects(d, "taps").Select(Tap.Parse).OfType<Tap>().ToList())).ToList(),
            Uncoupled = [.. Json.Ints(c, "uncoupled_inputs")],
            Outputs = Json.Objects(c, "sources").Where(s => Json.IntOrNull(s, "channel") is not null)
                .Select(s => new OutputPatch(Json.Int(s, "channel"), Json.Str(s, "name"), Json.Str(s, "format", "standard"),
                    s["device_channels"] is JsonArray ? Json.Ints(s, "device_channels") : null)).ToList(),
        };
    }
}

public sealed class DeviceMeters
{
    public List<double?> ToNet { get; } = [];
    public List<double?> FromNet { get; } = [];
    /// Received routes (1-based device channels) → jitter buffer primed.
    public List<(List<int> Channels, bool Primed)> Inputs { get; } = [];

    public static DeviceMeters From(JsonNode status)
    {
        var m = new DeviceMeters();
        JsonNode? dev = status["device"];
        if (dev is null)
        {
            return m;
        }
        if (dev["to_net_peak_dbfs"] is JsonArray a)
        {
            m.ToNet.AddRange(a.Select(Json.Double));
        }
        if (dev["from_net_peak_dbfs"] is JsonArray b)
        {
            m.FromNet.AddRange(b.Select(Json.Double));
        }
        foreach (JsonNode r in Json.Objects(dev, "inputs"))
        {
            m.Inputs.Add((Json.Ints(r, "device_channels"), Json.Bool(r, "primed")));
        }
        return m;
    }

    /// Maximum peak (dBFS) across 1-based channels, or null for silence.
    public static double? Peak(List<double?> values, IEnumerable<int> channels) =>
        channels.Where(c => c >= 1 && c <= values.Count).Select(c => values[c - 1]).Where(v => v is not null).Max();
}

/// Network connection: session interface, or searching.
public sealed record LinkStatus(bool Auto = true, bool Searching = true, string Iface = "", string Friendly = "", string Ipv4 = "")
{
    public static LinkStatus From(JsonNode s)
    {
        string iface = Json.Str(s, "iface");
        return new LinkStatus(Json.Bool(s, "iface_auto"), Json.Bool(s, "searching", iface.Length == 0), iface,
            Json.Str(s, "iface_friendly", iface), Json.Str(s, "ipv4"));
    }
}

public static class Livewire
{
    /// Multicast group of a Livewire channel (docs/protocol/01-channels.md).
    public static string Group(int channel, string kind)
    {
        int b = kind switch { "backfeed" => 193, "surround" => 196, _ => 192 };
        return $"239.{b}.{channel >> 8}.{channel & 0xFF}";
    }
}
