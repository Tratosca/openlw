// Input patch matrix: rows = sources (discovered, configured, manual), columns = device input
// channels, headed per pair (meters and state). A free cell asks the window for a patch (stereo on
// the pair, or one channel in mono); a patch is drawn as one block over its channels, with its
// tag, and a click on it releases it. The headphone button previews the source; ✕ removes a
// manual or unadvertised row. Same behavior as the macOS grid (duplex layout).
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using OpenLW.Daemon;

namespace OpenLW.Controls;

/// Patch drawn on a row: first column (0-based device channel), column count, tag (“L”, “R”,
/// “L+R”, “8”, or empty for a stereo patch).
public sealed record GridPatch(int Column, int Span, string Tag)
{
    public bool Covers(int column) => column >= Column && column < Column + Span;
}

public sealed record GridRow(DiscoveredSource Source, GridPatch? Patch, string Origin, bool Removable);

public sealed class InputGrid : Grid
{
    private const double ColumnWidth = 48;
    private const double Spacing = 4;
    private List<GridRow> rows = [];
    private List<List<int>> pairs = [];
    private int? listeningRow;
    private readonly List<Meter> columnMeters = [];
    private readonly List<TextBlock> columnStatus = [];
    private Meter? listenMeter;

    /// Cell click: row, column (0-based device channel), clicked element (menu anchor).
    public event Action<int, int, FrameworkElement>? Toggle;
    public event Action<int>? Listen;        // row
    public event Action<int>? Remove;        // row

    public InputGrid()
    {
        ColumnSpacing = Spacing;
        RowSpacing = 4;
    }

    /// Replaces rows, header pairs (1-based device channels) and preview row; rebuilds only
    /// when something changed.
    public void Update(List<GridRow> newRows, List<List<int>> newPairs, int? listening)
    {
        bool same = listening == listeningRow && newRows.SequenceEqual(rows) &&
                    newPairs.Count == pairs.Count && newPairs.Zip(pairs).All(p => p.First.SequenceEqual(p.Second));
        rows = newRows;
        pairs = newPairs;
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
        Children.Clear();
        ColumnDefinitions.Clear();
        RowDefinitions.Clear();
        columnMeters.Clear();
        columnStatus.Clear();
        listenMeter = null;
        int columns = pairs.Sum(p => p.Count);
        // Columns: preview, channel, name, origin, one per device channel, removal.
        foreach (double w in new double[] { 36, 60, 150, 150 })
        {
            ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(w) });
        }
        for (int c = 0; c < columns; c++)
        {
            ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(ColumnWidth) });
        }
        ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(32) });

        RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        foreach (List<int> pair in pairs)
        {
            double width = pair.Count * ColumnWidth + (pair.Count - 1) * Spacing;
            var head = new StackPanel { Spacing = 3, HorizontalAlignment = HorizontalAlignment.Center };
            head.Children.Add(Caption(pair.Count == 1 ? Loc.F("InputSingle", pair[0]) : Loc.F("InputsRange", pair[0], pair[^1]), center: true));
            var meter = new Meter(pair.Count, width - 16) { HorizontalAlignment = HorizontalAlignment.Center };
            columnMeters.Add(meter);
            head.Children.Add(meter);
            var st = Caption(Loc.S("StatusFree"), center: true);
            columnStatus.Add(st);
            head.Children.Add(st);
            Place(head, 0, 4 + pair[0] - 1, pair.Count);
        }
        if (rows.Count == 0)
        {
            RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            Place(Caption(Loc.S("NoSources")), 1, 0, 4 + columns);
            return;
        }
        for (int r = 0; r < rows.Count; r++)
        {
            int row = r;
            GridRow g = rows[r];
            RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            int y = r + 1;

            var listen = new Button
            {
                Content = new FontIcon { Glyph = "", FontSize = 14 }, // headphones
                Padding = new Thickness(6, 4, 6, 4),
            };
            ToolTipService.SetToolTip(listen, Loc.S(listeningRow == r ? "StopListening" : "ListenTooltip"));
            if (listeningRow == r)
            {
                listen.Style = (Style)Application.Current.Resources["AccentButtonStyle"];
            }
            listen.Click += (_, _) => Listen?.Invoke(row);
            Place(listen, y, 0);

            Place(new TextBlock { Text = g.Source.Channel.ToString(), FontFamily = new FontFamily("Consolas"), VerticalAlignment = VerticalAlignment.Center }, y, 1);
            var name = new StackPanel { VerticalAlignment = VerticalAlignment.Center };
            name.Children.Add(new TextBlock { Text = g.Source.Name.Length == 0 ? "—" : g.Source.Name, TextTrimming = TextTrimming.CharacterEllipsis });
            if (listeningRow == r)
            {
                listenMeter = new Meter(2, 120);
                name.Children.Add(listenMeter);
            }
            Place(name, y, 2);
            string kind = g.Source.PatchKind switch
            {
                "surround" => $" · {Loc.S("KindTagSurround")}",
                "backfeed" => $" · {Loc.S("KindTagBackfeed")}",
                _ => "",
            };
            Place(Caption(g.Origin + kind), y, 3);

            for (int c = 0; c < columns; c++)
            {
                if (g.Patch?.Covers(c) == true)
                {
                    continue;
                }
                int column = c;
                var cell = new Button { HorizontalAlignment = HorizontalAlignment.Stretch, Height = 30, Padding = new Thickness(0) };
                int ch = c + 1, pairFirst = ch % 2 == 0 ? ch - 1 : ch;
                ToolTipService.SetToolTip(cell, g.Source.PatchKind == "surround"
                    ? Loc.F("SendToInputs", g.Source.Channel, pairFirst, pairFirst + 7)
                    : Loc.F("SendToInput", g.Source.Channel, ch));
                cell.Click += (s, _) => Toggle?.Invoke(row, column, (FrameworkElement)s);
                Place(cell, y, 4 + c);
            }
            // Patch: one block over its channels, with its tag (a check mark for stereo).
            if (g.Patch is { } p && p.Column >= 0 && p.Column < columns)
            {
                int span = Math.Min(p.Span, columns - p.Column);
                var block = new Button
                {
                    Style = (Style)Application.Current.Resources["AccentButtonStyle"],
                    Content = p.Tag.Length == 0
                        ? new FontIcon { Glyph = "", FontSize = 12 } // check mark
                        : new TextBlock { Text = p.Tag, FontSize = 12 },
                    HorizontalAlignment = HorizontalAlignment.Stretch,
                    Height = 30,
                    Padding = new Thickness(0),
                };
                ToolTipService.SetToolTip(block, span == 1
                    ? Loc.F("ReleaseInput", p.Column + 1)
                    : Loc.F("ReleaseInputs", p.Column + 1, p.Column + span));
                block.Click += (s, _) => Toggle?.Invoke(row, p.Column, (FrameworkElement)s);
                Place(block, y, 4 + p.Column, span);
            }
            if (g.Removable)
            {
                var remove = new Button { Content = "✕", Padding = new Thickness(6, 2, 6, 2) };
                ToolTipService.SetToolTip(remove, Loc.S("RemoveFromGrid"));
                remove.Click += (_, _) => Remove?.Invoke(row);
                Place(remove, y, 4 + columns);
            }
        }
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

    private void Place(FrameworkElement e, int row, int column, int span = 1)
    {
        SetRow(e, row);
        SetColumn(e, column);
        if (span > 1)
        {
            SetColumnSpan(e, span);
        }
        Children.Add(e);
    }
}
