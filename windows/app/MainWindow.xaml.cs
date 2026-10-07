// Main window logic, ported from macos/app/Sources/MainWindowController.swift.
// Polls status at 5 Hz; configuration, sources and interfaces every 2 s.
using System.Text.Json.Nodes;
using Microsoft.UI;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using OpenLW.Audio;
using OpenLW.Controls;
using OpenLW.Daemon;
using Windows.Graphics;
using Windows.System;
using Windows.UI;

namespace OpenLW;

public sealed partial class MainWindow : Window
{
    private const int MaxPairs = 16;
    private static readonly (string Value, string Title)[] Latencies =
    [
        ("low", "Low: ≈ 8 ms added, dedicated network"),
        ("normal", "Normal: ≈ 17 ms added"),
        ("safe", "Safe: ≈ 35 ms added, shared network or busy computer"),
    ];
    private static readonly (int Dscp, string Title)[] Dscps =
    [
        (46, "EF (46): Livewire default"),
        (34, "AF41 (34): recommended for AES67"),
        (0, "None (0)"),
    ];

    private readonly DaemonClient client = new();
    private readonly Listener listener = new();
    private readonly AppSettings settings = AppSettings.Load();
    private readonly InputGrid grid = new();
    private readonly List<OutputRowView> outputRows = [];
    private readonly HashSet<string> busy = [];
    private readonly Microsoft.UI.Dispatching.DispatcherQueueTimer fast, slow;

    private DaemonConfig config = new();
    private bool configLoaded;
    private DeviceMeters meters = new();
    private List<DiscoveredSource> discovered = [];
    private List<Iface> ifaces = [];
    private LinkStatus link = new();
    private bool reachable;
    private List<GridRow> gridRows = [];
    private List<List<int>> pairs = [];
    private (int Channel, string Kind)? listening;
    /// Programmatic control updates must not be taken as user choices.
    private bool updating;

    public MainWindow()
    {
        InitializeComponent();
        ExtendsContentIntoTitleBar = true;
        SetTitleBar(TitleBarText);
        AppWindow.Resize(new SizeInt32(980, 820));
        AppWindow.Title = "OpenLW";
        AsioLogo.Source = new BitmapImage(new Uri(Path.Combine(AppContext.BaseDirectory, "Assets", "asio-compatible-logo.png")));

        updating = true;
        foreach (ComboBox c in new[] { InCount, OutCount })
        {
            string unit = c == InCount ? "inputs" : "outputs";
            for (int n = 1; n <= MaxPairs; n++)
            {
                c.Items.Add($"{n} {(n == 1 ? "channel" : "channels")} ({2 * n} {unit})");
            }
        }
        foreach (var l in Latencies)
        {
            LatencyCombo.Items.Add(l.Title);
        }
        foreach (var d in Dscps)
        {
            DscpCombo.Items.Add(d.Title);
        }
        AdvancedExpander.IsExpanded = settings.AdvancedVisible;
        updating = false;

        grid.Toggle += ToggleInput;
        grid.Listen += ToggleListen;
        grid.Remove += RemoveRow;
        GridHost.Content = grid;
        RebuildOutputRows(1);

        fast = DispatcherQueue.CreateTimer();
        fast.Interval = TimeSpan.FromMilliseconds(200);
        fast.Tick += (_, _) => RefreshStatus();
        slow = DispatcherQueue.CreateTimer();
        slow.Interval = TimeSpan.FromSeconds(2);
        slow.Tick += (_, _) => RefreshSlow();
        Activated += FirstActivation;
        Closed += (_, _) =>
        {
            fast.Stop();
            slow.Stop();
            listener.Dispose();
            client.Dispose();
        };
    }

    private void FirstActivation(object sender, WindowActivatedEventArgs e)
    {
        Activated -= FirstActivation;
        RefreshSlow();
        RefreshStatus();
        fast.Start();
        slow.Start();
    }

    // ---------- Polling ----------

    /// Sends `request` unless a request of the same kind is still pending.
    private async void Poll(string key, JsonObject request, Action<JsonObject> handle)
    {
        if (!busy.Add(key))
        {
            return;
        }
        DaemonResult r = await client.CallAsync(request);
        busy.Remove(key);
        if (r.Ok)
        {
            handle(r.Reply!);
        }
        else if (r.Error!.Kind == DaemonErrorKind.Unreachable)
        {
            SetReachable(false);
        }
    }

    private void RefreshStatus() => Poll("status", Cmd("status"), reply =>
    {
        if (reply["status"] is not JsonObject status)
        {
            return;
        }
        SetReachable(true);
        meters = DeviceMeters.From(status);
        LinkStatus next = LinkStatus.From(status);
        if (listening is not null && (next.Iface != link.Iface || next.Ipv4 != link.Ipv4 || next.Searching))
        {
            StopListening(); // interface changed: group membership no longer valid
        }
        bool changed = next.Searching != link.Searching || next.Iface != link.Iface || next.Auto != link.Auto;
        link = next;
        ShowLink();
        if (changed)
        {
            UpdateIfaceCombo();
        }
        UpdateMeters();
    });

    private void RefreshSlow()
    {
        Poll("config", Cmd("config"), reply =>
        {
            if (reply["config"] is JsonObject c)
            {
                ApplyConfig(DaemonConfig.From(c));
            }
        });
        Poll("sources", Cmd("sources"), reply =>
        {
            discovered = Json.Objects(reply, "sources").Select(DiscoveredSource.From).OfType<DiscoveredSource>().ToList();
            UpdateGrid();
        });
        Poll("ifaces", Cmd("ifaces"), reply =>
        {
            ifaces = Json.Objects(reply, "ifaces").Select(Iface.From).OfType<Iface>().ToList();
            UpdateIfaceCombo();
        });
    }

    private void SetReachable(bool ok)
    {
        if (ok == reachable && ok)
        {
            return;
        }
        reachable = ok;
        if (!ok)
        {
            StateDot.Fill = Brush(0xFF, 0x3B, 0x30);
            StateText.Text = new DaemonError(DaemonErrorKind.Unreachable).Message;
        }
    }

    /// Status line: active interface, or network search.
    private void ShowLink()
    {
        if (link.Searching)
        {
            StateDot.Fill = Brush(0xFF, 0x9F, 0x0A);
            StateText.Text = link.Auto
                ? "Searching for the Livewire network. Connect the computer to the Livewire network, or choose the interface."
                : $"Interface “{config.Iface}” unavailable. Connect it, or choose Automatic.";
            return;
        }
        StateDot.Fill = Brush(0x34, 0xC7, 0x59);
        string name = link.Friendly == link.Iface ? link.Iface : $"{link.Friendly} ({link.Iface})";
        StateText.Text = $"Connected to the Livewire network via {name} · {link.Ipv4}" +
                         (link.Auto ? " · interface chosen automatically" : "");
    }

    // ---------- Display updates ----------

    private void ApplyConfig(DaemonConfig c)
    {
        bool pairsChanged = !configLoaded || c.ChannelsToNet != config.ChannelsToNet;
        config = c;
        configLoaded = true;
        updating = true;
        AdvertiseCheck.IsChecked = c.Advertise;
        if (!ReferenceEquals(FocusManager.GetFocusedElement(Content.XamlRoot), TerminalName))
        {
            TerminalName.Text = c.TerminalName;
        }
        LatencyCombo.SelectedIndex = Array.FindIndex(Latencies, l => l.Value == c.Latency);
        DscpCombo.SelectedIndex = Array.FindIndex(Dscps, d => d.Dscp << 2 == c.Tos); // -1: value outside list
        InCount.SelectedIndex = Math.Clamp(c.ChannelsFromNet / 2, 1, MaxPairs) - 1;
        OutCount.SelectedIndex = Math.Clamp(c.ChannelsToNet / 2, 1, MaxPairs) - 1;
        updating = false;
        if (pairsChanged)
        {
            RebuildOutputRows(Math.Max(1, c.ChannelsToNet / 2));
        }
        pairs = Enumerable.Range(0, c.ChannelsFromNet / 2).Select(i => new List<int> { 2 * i + 1, 2 * i + 2 }).ToList();
        foreach (OutputRowView row in outputRows)
        {
            row.Show(c.Outputs.FirstOrDefault(o => o.DeviceChannels is not null && o.DeviceChannels.SequenceEqual(row.Pair)));
        }
        UpdateIfaceCombo();
        UpdateGrid();
    }

    /// Menu: automatic selection, then Ethernet interfaces (plus the configured one if not Ethernet).
    private void UpdateIfaceCombo()
    {
        bool auto = config.AutoIface;
        string autoTitle = !auto ? "Automatic" : link.Searching ? "Automatic · searching" : $"Automatic · {link.Friendly}";
        var items = new List<(string Title, string Name)> { (autoTitle, "auto") };
        items.AddRange(ifaces.Where(i => i.Candidate || (!auto && i.Name == config.Iface))
            .OrderBy(i => i.Livewire ? 0 : 1).ThenBy(i => i.Name).Select(i => (i.Title, i.Name)));
        if (!auto && ifaces.All(i => i.Name != config.Iface))
        {
            items.Add(($"{config.Iface} (unavailable)", config.Iface));
        }
        updating = true;
        var existing = IfaceCombo.Items.OfType<ComboBoxItem>().Select(i => $"{i.Content}|{i.Tag}").ToList();
        if (!existing.SequenceEqual(items.Select(i => $"{i.Title}|{i.Name}")))
        {
            IfaceCombo.Items.Clear();
            foreach (var (title, name) in items)
            {
                IfaceCombo.Items.Add(new ComboBoxItem { Content = title, Tag = name });
            }
        }
        string wanted = auto ? "auto" : config.Iface;
        IfaceCombo.SelectedItem = IfaceCombo.Items.OfType<ComboBoxItem>().FirstOrDefault(i => (string)i.Tag == wanted);
        updating = false;
    }

    private int? PatchedColumn(int channel, string kind)
    {
        InputPatch? p = config.Inputs.FirstOrDefault(i => i.Channel == channel && i.Kind == kind && i.DeviceChannels.Count > 0);
        return p is null ? null : (p.DeviceChannels[0] - 1) / 2;
    }

    /// Matrix rows: discovered sources, then manual entries, then configured unadvertised streams.
    private void UpdateGrid()
    {
        var rows = new List<GridRow>();
        var seen = new HashSet<string>();
        void Add(DiscoveredSource s, string origin, bool removable)
        {
            if (seen.Add($"{s.Channel}/{s.PatchKind}"))
            {
                rows.Add(new GridRow(s, PatchedColumn(s.Channel, s.PatchKind), origin, removable));
            }
        }
        foreach (DiscoveredSource s in discovered)
        {
            Add(s, s.Terminal, false);
        }
        foreach (ManualSource m in settings.Manual)
        {
            Add(new DiscoveredSource(m.Channel, "", "", m.Kind, ""), "manual", true);
        }
        foreach (InputPatch p in config.Inputs.Where(p => p.Channel is not null))
        {
            Add(new DiscoveredSource(p.Channel!.Value, "", "", p.Kind, ""), "not advertised", true);
        }
        gridRows = rows;
        int idx = listening is { } l ? rows.FindIndex(r => r.Source.Channel == l.Channel && r.Source.PatchKind == l.Kind) : -1;
        grid.Update(rows, pairs, idx >= 0 ? idx : null);
        UpdateMeters();
    }

    private void UpdateMeters()
    {
        var columns = pairs.Select(p => (IReadOnlyList<double?>)p.Select(c => DeviceMeters.Peak(meters.FromNet, [c])).ToList()).ToList();
        var status = pairs.Select(p =>
        {
            var route = meters.Inputs.FirstOrDefault(r => r.Channels.Contains(p[0]));
            return route.Channels is null ? "free" : route.Primed ? "receiving audio" : "waiting";
        }).ToList();
        grid.ShowLevels(columns, status, listening is null ? null : listener.TakePeak());
        foreach (OutputRowView row in outputRows)
        {
            row.Meter.Show(row.Pair.Select(c => DeviceMeters.Peak(meters.ToNet, [c])).ToList());
        }
    }

    private void ShowError(DaemonError? error) => ShowMessage(error?.Message);

    private void ShowMessage(string? message)
    {
        MessageBar.Message = message ?? "";
        MessageBar.IsOpen = message is not null;
    }

    private void RebuildOutputRows(int count)
    {
        OutputRows.Children.Clear();
        outputRows.Clear();
        for (int i = 0; i < count; i++)
        {
            var row = new OutputRowView([2 * i + 1, 2 * i + 2]);
            row.Apply += ApplyOutput;
            outputRows.Add(row);
            OutputRows.Children.Add(row);
        }
    }

    // ---------- Actions ----------

    /// Modification request; the returned configuration replaces the displayed state.
    private async void Mutate(JsonObject request)
    {
        DaemonResult r = await client.CallAsync(request);
        if (r.Ok)
        {
            ShowError(null);
            if (r.Reply!["config"] is JsonObject c)
            {
                ApplyConfig(DaemonConfig.From(c));
            }
        }
        else
        {
            ShowError(r.Error);
            RefreshSlow();
        }
    }

    private void IfaceChanged(object sender, SelectionChangedEventArgs e)
    {
        if (updating || IfaceCombo.SelectedItem is not ComboBoxItem item)
        {
            return;
        }
        string name = (string)item.Tag;
        if (name == "auto" ? config.AutoIface : name == config.Iface)
        {
            return;
        }
        Mutate(Cmd("set_iface", ("iface", name)));
    }

    private void AdvertiseClicked(object sender, RoutedEventArgs e) =>
        Mutate(Cmd("set_advertise", ("advertise", AdvertiseCheck.IsChecked == true)));

    private async void ChannelCountChanged(object sender, SelectionChangedEventArgs e)
    {
        if (updating || !configLoaded)
        {
            return;
        }
        int toNet = 2 * (OutCount.SelectedIndex + 1), fromNet = 2 * (InCount.SelectedIndex + 1);
        if (toNet == config.ChannelsToNet && fromNet == config.ChannelsFromNet)
        {
            return;
        }
        int lostOut = config.Outputs.Count(o => (o.DeviceChannels?.DefaultIfEmpty(0).Max() ?? 0) > toNet);
        int lostIn = config.Inputs.Count(i => i.DeviceChannels.DefaultIfEmpty(0).Max() > fromNet);
        if (lostOut + lostIn > 0)
        {
            var lost = new List<string>();
            if (lostOut > 0)
            {
                lost.Add($"{lostOut} transmission{(lostOut > 1 ? "s" : "")} stopped");
            }
            if (lostIn > 0)
            {
                lost.Add($"{lostIn} source{(lostIn > 1 ? "s" : "")} removed from the inputs");
            }
            var dialog = new ContentDialog
            {
                XamlRoot = Content.XamlRoot,
                Title = "Reduce the number of channels?",
                Content = string.Join(", ", lost) + ". The software using the OpenLW driver must reset it.",
                PrimaryButtonText = "Reduce",
                CloseButtonText = "Cancel",
                DefaultButton = ContentDialogButton.Close,
            };
            if (await dialog.ShowAsync() != ContentDialogResult.Primary)
            {
                ApplyConfig(config);
                return;
            }
        }
        Mutate(Cmd("set_device_channels", ("to_net", toNet), ("from_net", fromNet)));
    }

    private void ManualKeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key == VirtualKey.Enter)
        {
            AddManualClicked(sender, e);
        }
    }

    private void AddManualClicked(object sender, RoutedEventArgs e)
    {
        if (!int.TryParse(ManualChannel.Text.Trim(), out int ch) || ch < 1 || ch > 32766)
        {
            ShowError(new DaemonError(DaemonErrorKind.Refused, "invalid channel. Enter a number from 1 to 32766."));
            return;
        }
        string kind = new[] { "stereo", "backfeed", "surround" }[Math.Max(0, ManualKind.SelectedIndex)];
        if (!settings.Manual.Any(m => m.Channel == ch && m.Kind == kind))
        {
            settings.Manual.Add(new ManualSource { Channel = ch, Kind = kind });
            settings.Save();
        }
        ManualChannel.Text = "";
        ShowError(null);
        UpdateGrid();
    }

    private void ToggleInput(int row, int column)
    {
        if (row >= gridRows.Count || column >= pairs.Count)
        {
            return;
        }
        GridRow r = gridRows[row];
        List<int> pair = pairs[column];
        if (r.PatchedColumn == column)
        {
            Mutate(Cmd("unpatch_input", ("device_channels", Ints(pair))));
            return;
        }
        int width = r.Source.PatchKind == "surround" ? 8 : 2, first = pair[0];
        if (first + width - 1 > config.ChannelsFromNet)
        {
            ShowError(new DaemonError(DaemonErrorKind.Refused,
                $"a surround source occupies 8 inputs. Choose a pair from 1-2 to {config.ChannelsFromNet - 7}-{config.ChannelsFromNet - 6}."));
            UpdateGrid();
            return;
        }
        Mutate(Cmd("patch_input", ("channel", r.Source.Channel), ("kind", r.Source.PatchKind),
            ("device_channels", Ints(Enumerable.Range(first, width)))));
    }

    /// Removes a manual or unadvertised row; releases its inputs if patched.
    private void RemoveRow(int row)
    {
        if (row >= gridRows.Count)
        {
            return;
        }
        DiscoveredSource s = gridRows[row].Source;
        if (listening is { } l && l.Channel == s.Channel && l.Kind == s.PatchKind)
        {
            StopListening();
        }
        settings.Manual.RemoveAll(m => m.Channel == s.Channel && (m.Kind is "stereo" or "backfeed" or "surround" ? m.Kind : "stereo") == s.PatchKind);
        settings.Save();
        // A received stream stays in the configuration, even unpatched (displaced by another
        // patch): without remove_input the row would come back as "not advertised".
        if (config.Inputs.Any(i => i.Channel == s.Channel && i.Kind == s.PatchKind))
        {
            Mutate(Cmd("remove_input", ("channel", s.Channel), ("kind", s.PatchKind)));
        }
        else
        {
            UpdateGrid();
        }
    }

    private void ToggleListen(int row)
    {
        if (row >= gridRows.Count)
        {
            return;
        }
        DiscoveredSource s = gridRows[row].Source;
        if (listening is { } l && l.Channel == s.Channel && l.Kind == s.PatchKind)
        {
            StopListening();
            return;
        }
        if (link.Searching || link.Ipv4.Length == 0)
        {
            ShowError(new DaemonError(DaemonErrorKind.Refused,
                "the computer isn't connected to the Livewire network yet. Choose the interface, then try again."));
            return;
        }
        string group = s.Stream.Length == 0 ? Livewire.Group(s.Channel, s.PatchKind) : s.Stream;
        try
        {
            listener.Start(group, link.Ipv4, s.PatchKind == "surround" ? 8 : 2, s.Kind == "stereo-l16" ? 16 : 24);
            listening = (s.Channel, s.PatchKind);
            ShowError(null);
        }
        catch (ListenError e)
        {
            listening = null;
            ShowMessage(e.Message);
        }
        UpdateGrid();
    }

    private void StopListening()
    {
        listener.Stop();
        listening = null;
        UpdateGrid();
    }

    private async void ApplyOutput(OutputRowView row)
    {
        OutputPatch? previous = config.Outputs.FirstOrDefault(o => o.DeviceChannels is not null && o.DeviceChannels.SequenceEqual(row.Pair));
        if (!row.Enabled)
        {
            if (previous is not null)
            {
                Mutate(Cmd("unpatch_output", ("channel", previous.Channel)));
            }
            return;
        }
        if (row.Channel is not int ch)
        {
            ShowError(new DaemonError(DaemonErrorKind.Refused, "invalid channel. Enter a number from 1 to 32766."));
            row.Show(previous);
            return;
        }
        // Channel change: stop this pair's previous stream first.
        if (previous is not null && previous.Channel != ch)
        {
            DaemonResult r = await client.CallAsync(Cmd("unpatch_output", ("channel", previous.Channel)));
            if (!r.Ok)
            {
                ShowError(r.Error);
                return;
            }
        }
        Mutate(Cmd("patch_output", ("channel", ch), ("name", row.StreamName), ("format", row.Format),
            ("device_channels", Ints(row.Pair))));
    }

    private void TerminalKeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key == VirtualKey.Enter)
        {
            CommitTerminalName();
        }
    }

    private void TerminalLostFocus(object sender, RoutedEventArgs e) => CommitTerminalName();

    private void CommitTerminalName()
    {
        string name = TerminalName.Text.Trim();
        if (configLoaded && name != config.TerminalName)
        {
            Mutate(Cmd("set_advanced", ("terminal_name", name)));
        }
    }

    private void LatencyChanged(object sender, SelectionChangedEventArgs e)
    {
        if (updating || LatencyCombo.SelectedIndex < 0)
        {
            return;
        }
        string value = Latencies[LatencyCombo.SelectedIndex].Value;
        if (value != config.Latency)
        {
            Mutate(Cmd("set_advanced", ("latency", value)));
        }
    }

    private void DscpChanged(object sender, SelectionChangedEventArgs e)
    {
        if (updating || DscpCombo.SelectedIndex < 0)
        {
            return;
        }
        int dscp = Dscps[DscpCombo.SelectedIndex].Dscp;
        if (dscp << 2 != config.Tos)
        {
            Mutate(Cmd("set_advanced", ("dscp", dscp)));
        }
    }

    private void AdvancedExpanding(Expander sender, ExpanderExpandingEventArgs args)
    {
        settings.AdvancedVisible = true;
        settings.Save();
    }

    private void AdvancedCollapsed(Expander sender, ExpanderCollapsedEventArgs args)
    {
        settings.AdvancedVisible = false;
        settings.Save();
    }

    // ---------- Helpers ----------

    private static JsonObject Cmd(string cmd, params (string Key, JsonNode? Value)[] args)
    {
        var o = new JsonObject { ["cmd"] = cmd };
        foreach (var (k, v) in args)
        {
            o[k] = v;
        }
        return o;
    }

    private static JsonArray Ints(IEnumerable<int> values) => new(values.Select(v => (JsonNode?)JsonValue.Create(v)).ToArray());

    private static SolidColorBrush Brush(byte r, byte g, byte b) => new(Color.FromArgb(255, r, g, b));
}

/// Output row: device pair, meter, channel, advertised name, format, transmission.
public sealed class OutputRowView : StackPanel
{
    private static readonly (string Value, string Title)[] Formats =
    [
        ("standard", "Standard (5 ms)"),
        ("aes67", "AES67 (1 ms)"),
        ("livestream", "Livestream (0.25 ms)"),
    ];

    private readonly TextBox channelBox = new() { PlaceholderText = "channel", Width = 90 };
    private readonly TextBox nameBox = new() { PlaceholderText = "advertised name", Width = 200 };
    private readonly ComboBox formatCombo = new() { MinWidth = 180 };
    private readonly CheckBox emitCheck = new() { Content = "Transmit" };
    private bool showing;

    public List<int> Pair { get; }
    public Meter Meter { get; } = new(2, 110);
    public event Action<OutputRowView>? Apply;

    public OutputRowView(List<int> pair)
    {
        Pair = pair;
        Orientation = Orientation.Horizontal;
        Spacing = 10;
        Children.Add(new TextBlock { Text = $"Outputs {pair[0]}-{pair[1]}", Width = 90, VerticalAlignment = VerticalAlignment.Center });
        Children.Add(Meter);
        foreach (var f in Formats)
        {
            formatCombo.Items.Add(f.Title);
        }
        formatCombo.SelectedIndex = 0;
        channelBox.FontFamily = new FontFamily("Consolas");
        foreach (TextBox box in new[] { channelBox, nameBox })
        {
            box.KeyDown += (_, e) =>
            {
                if (e.Key == VirtualKey.Enter && Enabled)
                {
                    Apply?.Invoke(this);
                }
            };
        }
        formatCombo.SelectionChanged += (_, _) =>
        {
            if (!showing && Enabled)
            {
                Apply?.Invoke(this);
            }
        };
        emitCheck.Click += (_, _) => Apply?.Invoke(this);
        Children.Add(channelBox);
        Children.Add(nameBox);
        Children.Add(formatCombo);
        Children.Add(emitCheck);
    }

    public bool Enabled => emitCheck.IsChecked == true;

    public int? Channel => int.TryParse(channelBox.Text.Trim(), out int n) && n >= 1 && n <= 32766 ? n : null;

    public string StreamName
    {
        get
        {
            string n = nameBox.Text.Trim();
            return n.Length == 0 ? $"PC {Pair[0]}-{Pair[1]}" : n;
        }
    }

    public string Format => Formats[Math.Max(0, formatCombo.SelectedIndex)].Value;

    /// Shows the configured state, except while the user is editing a field.
    public void Show(OutputPatch? patch)
    {
        if (channelBox.FocusState != FocusState.Unfocused || nameBox.FocusState != FocusState.Unfocused)
        {
            return;
        }
        showing = true;
        emitCheck.IsChecked = patch is not null;
        if (patch is not null)
        {
            channelBox.Text = patch.Channel.ToString();
            nameBox.Text = patch.Name;
            int idx = Array.FindIndex(Formats, f => f.Value == patch.Format);
            if (idx >= 0)
            {
                formatCombo.SelectedIndex = idx;
            }
        }
        showing = false;
    }
}
