// Input patch matrix: rows = sources (discovered, configured, manual), columns = driver input
// channels, headed per pair (title, coupling link, meters, state). Same behavior as the macOS
// grid in the duplex layout (macos/app/Sources/Views.swift, InputMatrixView):
// - a coupled pair (the default) is one target: a click patches the source in stereo there
//   (surround: 8 inputs from the pair start), a click on a patched pair releases it;
// - the link button in a pair header couples or uncouples it;
// - on an uncoupled pair each input has two halves, top = left (L), bottom = right (R): a
//   click adds or removes that side of the source on the input (both: L+R); surround rows
//   keep one target per input, which acts like a pair click.
// The source columns (preview, channel, name, origin) stay fixed on the left; the cells scroll
// horizontally when the pairs do not fit, so the window keeps its width.
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using OpenLW.Daemon;

namespace OpenLW.Controls;

/// Header group: a pair of driver inputs (0-based first column, column count), coupled in
/// stereo or not.
public sealed record GridHeader(int First, int Count, string Title, bool Coupled)
{
    public bool Contains(int column) => column >= First && column < First + Count;
}

/// Sides of a stereo source feeding an uncoupled input.
public sealed record GridSides(bool Left, bool Right);

/// Matrix row: source and its crosspoints. `Groups`: coupled pairs (and, for a surround
/// source, every pair) fed by it, header index → label (empty: plain stereo). `Sides`:
/// uncoupled inputs (0-based column) fed by it. `Block`: 1-based first and last input of a
/// surround patch (released as a whole).
public sealed record GridRow(DiscoveredSource Source, IReadOnlyDictionary<int, string> Groups,
    IReadOnlyDictionary<int, GridSides> Sides, (int First, int Last)? Block, string Origin, bool Removable)
{
    public bool Equals(GridRow? other) =>
        other is not null && Source == other.Source && Block == other.Block && Origin == other.Origin &&
        Removable == other.Removable && Same(Groups, other.Groups) && Same(Sides, other.Sides);

    public override int GetHashCode() => HashCode.Combine(Source, Block, Origin, Removable, Groups.Count, Sides.Count);

    private static bool Same<T>(IReadOnlyDictionary<int, T> a, IReadOnlyDictionary<int, T> b) =>
        a.Count == b.Count && a.All(kv => b.TryGetValue(kv.Key, out T? v) && EqualityComparer<T>.Default.Equals(kv.Value, v));
}

public sealed class InputGrid : Grid
{
    private const double ColumnWidth = 50;
    private const double Spacing = 4;
    private const double HeaderHeight = 64;
    private const double RowHeight = 34;
    private const string CheckGlyph = "";
    private const string HeadphonesGlyph = "";
    private const string LinkGlyph = "";

    private readonly Grid labels = new() { ColumnSpacing = Spacing, RowSpacing = Spacing };
    /// Bottom padding: room for the horizontal scroll bar, drawn over the content.
    private readonly Grid cells = new() { ColumnSpacing = Spacing, RowSpacing = Spacing, Padding = new Thickness(0, 0, 0, 14) };
    private List<GridRow> rows = [];
    private List<GridHeader> headers = [];
    private int? listeningRow;
    private readonly List<Meter> columnMeters = [];
    private readonly List<TextBlock> columnStatus = [];
    private Meter? listenMeter;

    /// Click on a coupled pair, or on a surround row: row, header index.
    public event Action<int, int>? Group;
    /// Click on one half of an uncoupled input: row, column (0-based input), left half?
    public event Action<int, int, bool>? Side;
    /// Click on a header's link button: header index.
    public event Action<int>? Coupling;
    public event Action<int>? Listen;        // row
    public event Action<int>? Remove;        // row

    public InputGrid()
    {
        ColumnSpacing = Spacing;
        ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        var scroll = new ScrollViewer
        {
            HorizontalScrollMode = ScrollMode.Enabled,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Auto,
            VerticalScrollMode = ScrollMode.Disabled,
            VerticalScrollBarVisibility = ScrollBarVisibility.Disabled,
            Content = cells,
        };
        SetColumn(scroll, 1);
        Children.Add(labels);
        Children.Add(scroll);
    }

    /// Replaces rows, header pairs and preview row; rebuilds only when something changed.
    public void Update(List<GridRow> newRows, List<GridHeader> newHeaders, int? listening)
    {
        bool same = listening == listeningRow && newRows.SequenceEqual(rows) && newHeaders.SequenceEqual(headers);
        rows = newRows;
        headers = newHeaders;
        listeningRow = listening;
        if (!same)
        {
            Rebuild();
        }
    }

    /// Levels and state per header pair.
    public void ShowLevels(List<IReadOnlyList<double?>> groups, List<string> status, double? listenLevel)
    {
        for (int i = 0; i < columnMeters.Count && i < groups.Count; i++)
        {
            columnMeters[i].Show(groups[i]);
        }
        for (int i = 0; i < columnStatus.Count && i < status.Count; i++)
        {
            columnStatus[i].Text = status[i];
        }
        listenMeter?.Show([listenLevel, listenLevel]);
    }

    private void Rebuild()
    {
        foreach (Grid g in new[] { labels, cells })
        {
            g.Children.Clear();
            g.ColumnDefinitions.Clear();
            g.RowDefinitions.Clear();
            g.RowDefinitions.Add(new RowDefinition { Height = new GridLength(HeaderHeight) });
        }
        columnMeters.Clear();
        columnStatus.Clear();
        listenMeter = null;
        int columns = headers.Sum(h => h.Count);
        // Labels: preview, channel, name, origin. Cells: one per input, removal.
        foreach (double w in new double[] { 36, 60, 150, 150 })
        {
            labels.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(w) });
        }
        for (int c = 0; c < columns; c++)
        {
            cells.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(ColumnWidth) });
        }
        cells.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(32) });

        for (int h = 0; h < headers.Count; h++)
        {
            Place(cells, Header(h), 0, headers[h].First, headers[h].Count);
        }
        if (rows.Count == 0)
        {
            labels.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            TextBlock empty = Caption(Loc.S("NoSources"));
            empty.TextWrapping = TextWrapping.Wrap;
            Place(labels, empty, 1, 0, 4);
            return;
        }
        for (int r = 0; r < rows.Count; r++)
        {
            foreach (Grid g in new[] { labels, cells })
            {
                g.RowDefinitions.Add(new RowDefinition { Height = new GridLength(RowHeight) });
            }
            AddLabels(r);
            AddCells(r, columns);
        }
    }

    /// Pair header: title and link button, meters, state.
    private StackPanel Header(int h)
    {
        GridHeader g = headers[h];
        double width = g.Count * ColumnWidth + (g.Count - 1) * Spacing;
        var head = new StackPanel { Spacing = 3, VerticalAlignment = VerticalAlignment.Top };
        var top = new Grid();
        top.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        top.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        top.Children.Add(Caption(g.Title, center: true));
        Button link = LinkButton(g.Coupled);
        link.Click += (_, _) => Coupling?.Invoke(h);
        SetColumn(link, 1);
        top.Children.Add(link);
        head.Children.Add(top);
        var meter = new Meter(g.Count, Math.Max(20, width - 16)) { HorizontalAlignment = HorizontalAlignment.Center };
        columnMeters.Add(meter);
        head.Children.Add(meter);
        TextBlock st = Caption(Loc.S("StatusFree"), center: true);
        columnStatus.Add(st);
        head.Children.Add(st);
        return head;
    }

    /// Link button: accent link while coupled, struck-through link once uncoupled.
    private static Button LinkButton(bool coupled)
    {
        Brush color = (Brush)Application.Current.Resources[coupled ? "AccentTextFillColorPrimaryBrush" : "TextFillColorTertiaryBrush"];
        var icon = new Grid { Width = 14, Height = 14 };
        icon.Children.Add(new FontIcon { Glyph = LinkGlyph, FontSize = 12, Foreground = color });
        if (!coupled)
        {
            icon.Children.Add(new Line { X1 = 1, Y1 = 13, X2 = 13, Y2 = 1, Stroke = color, StrokeThickness = 1.3 });
        }
        var button = new Button
        {
            Content = icon,
            Padding = new Thickness(2),
            MinWidth = 0,
            MinHeight = 0,
            Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent),
            BorderThickness = new Thickness(0),
            VerticalAlignment = VerticalAlignment.Center,
        };
        string tip = Loc.S(coupled ? "Uncouple" : "Couple");
        ToolTipService.SetToolTip(button, tip);
        AutomationProperties.SetName(button, tip);
        return button;
    }

    /// Fixed part of a row: preview button, channel, name (and preview level), origin.
    private void AddLabels(int r)
    {
        int row = r;
        GridRow g = rows[r];
        int y = r + 1;
        var listen = new Button
        {
            Content = new FontIcon { Glyph = HeadphonesGlyph, FontSize = 14 },
            Padding = new Thickness(6, 4, 6, 4),
            VerticalAlignment = VerticalAlignment.Center,
        };
        string tip = Loc.S(listeningRow == r ? "StopListening" : "ListenTooltip");
        ToolTipService.SetToolTip(listen, tip);
        AutomationProperties.SetName(listen, tip);
        if (listeningRow == r)
        {
            listen.Style = (Style)Application.Current.Resources["AccentButtonStyle"];
        }
        listen.Click += (_, _) => Listen?.Invoke(row);
        Place(labels, listen, y, 0);

        Place(labels, new TextBlock { Text = g.Source.Channel.ToString(), FontFamily = new FontFamily("Consolas"), VerticalAlignment = VerticalAlignment.Center }, y, 1);
        var name = new StackPanel { VerticalAlignment = VerticalAlignment.Center };
        name.Children.Add(new TextBlock { Text = g.Source.Name.Length == 0 ? "—" : g.Source.Name, TextTrimming = TextTrimming.CharacterEllipsis });
        if (listeningRow == r)
        {
            listenMeter = new Meter(2, 120) { HorizontalAlignment = HorizontalAlignment.Left };
            name.Children.Add(listenMeter);
        }
        Place(labels, name, y, 2);
        string kind = g.Source.PatchKind switch
        {
            "surround" => $" · {Loc.S("KindTagSurround")}",
            "backfeed" => $" · {Loc.S("KindTagBackfeed")}",
            _ => "",
        };
        Place(labels, Caption(g.Origin + kind), y, 3);
    }

    /// Scrolling part of a row: crosspoints per pair, removal button.
    private void AddCells(int r, int columns)
    {
        int row = r;
        GridRow g = rows[r];
        int y = r + 1;
        int ch = g.Source.Channel;
        bool surround = g.Source.PatchKind == "surround";
        for (int h = 0; h < headers.Count; h++)
        {
            int header = h;
            GridHeader p = headers[h];
            if (p.Coupled || surround)
            {
                // One target over the pair (coupled), or one per input (surround, uncoupled).
                string? tag = g.Groups.TryGetValue(h, out string? t) ? t : null;
                string tip = tag is not null
                    ? surround && g.Block is { } b ? Loc.F("ReleaseInputs", b.First, b.Last)
                    : p.Count == 1 ? Loc.F("ReleaseInput", p.First + 1) : Loc.F("ReleaseInputs", p.First + 1, p.First + p.Count)
                    : surround ? Loc.F("SendToInputs", ch, p.First + 1, p.First + 8)
                    : p.Count == 1 ? Loc.F("SendToInput", ch, p.First + 1) : Loc.F("SendToInputs", ch, p.First + 1, p.First + p.Count);
                List<(int First, int Span)> spans = p.Coupled ? [(p.First, p.Count)]
                    : [.. Enumerable.Range(p.First, p.Count).Select(c => (c, 1))];
                foreach (var (first, span) in spans)
                {
                    Button cell = GroupButton(tag, tip);
                    cell.Click += (_, _) => Group?.Invoke(row, header);
                    Place(cells, cell, y, first, span);
                }
                continue;
            }
            for (int c = p.First; c < p.First + p.Count; c++)
            {
                int column = c;
                GridSides sides = g.Sides.TryGetValue(c, out GridSides? s) ? s : new GridSides(false, false);
                var halves = new StackPanel { Spacing = 2, VerticalAlignment = VerticalAlignment.Center };
                foreach (bool left in new[] { true, false })
                {
                    bool isLeft = left;
                    ToggleButton half = HalfButton(Loc.S(left ? "TagLeft" : "TagRight"), left ? sides.Left : sides.Right,
                        Loc.F(left ? "SideLeftTooltip" : "SideRightTooltip", ch, c + 1));
                    half.Click += (_, _) => Side?.Invoke(row, column, isLeft);
                    halves.Children.Add(half);
                }
                Place(cells, halves, y, c);
            }
        }
        if (g.Removable)
        {
            var remove = new Button { Content = "✕", Padding = new Thickness(6, 2, 6, 2), VerticalAlignment = VerticalAlignment.Center };
            string tip = Loc.S("RemoveFromGrid");
            ToolTipService.SetToolTip(remove, tip);
            AutomationProperties.SetName(remove, tip);
            remove.Click += (_, _) => Remove?.Invoke(row);
            Place(cells, remove, y, columns);
        }
    }

    /// Pair (or surround input) target: neutral when free; accent when patched, with a check
    /// mark for plain stereo or the label of the crosspoints.
    private static Button GroupButton(string? tag, string tip)
    {
        var button = new Button
        {
            HorizontalAlignment = HorizontalAlignment.Stretch,
            VerticalAlignment = VerticalAlignment.Center,
            Height = 30,
            Padding = new Thickness(0),
        };
        if (tag is not null)
        {
            button.Style = (Style)Application.Current.Resources["AccentButtonStyle"];
            button.Content = tag.Length == 0
                ? new FontIcon { Glyph = CheckGlyph, FontSize = 12 }
                : new TextBlock { Text = tag, FontSize = 12, TextTrimming = TextTrimming.CharacterEllipsis };
        }
        ToolTipService.SetToolTip(button, tip);
        AutomationProperties.SetName(button, tip);
        return button;
    }

    /// Half of an uncoupled input: left (top) or right (bottom) side of the source. A toggle
    /// button (state exposed to screen readers) that only shows the configuration: a click
    /// sends the request, the reply redraws the grid.
    private static ToggleButton HalfButton(string label, bool on, string tip)
    {
        var button = new ToggleButton
        {
            Content = new TextBlock
            {
                Text = label,
                FontSize = 10,
                LineHeight = 12,
                LineStackingStrategy = LineStackingStrategy.BlockLineHeight,
                VerticalAlignment = VerticalAlignment.Center,
            },
            IsChecked = on,
            HorizontalAlignment = HorizontalAlignment.Stretch,
            Height = 15,
            MinHeight = 0,
            Padding = new Thickness(0),
        };
        button.Click += (_, _) => button.IsChecked = on;
        ToolTipService.SetToolTip(button, tip);
        AutomationProperties.SetName(button, tip);
        return button;
    }

    private static TextBlock Caption(string text, bool center = false) => new()
    {
        Text = text,
        Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"],
        Foreground = (Brush)Application.Current.Resources["TextFillColorSecondaryBrush"],
        VerticalAlignment = VerticalAlignment.Center,
        HorizontalAlignment = center ? HorizontalAlignment.Center : HorizontalAlignment.Left,
        TextTrimming = TextTrimming.CharacterEllipsis,
    };

    private static void Place(Grid grid, FrameworkElement e, int row, int column, int span = 1)
    {
        SetRow(e, row);
        SetColumn(e, column);
        if (span > 1)
        {
            SetColumnSpan(e, span);
        }
        grid.Children.Add(e);
    }
}
