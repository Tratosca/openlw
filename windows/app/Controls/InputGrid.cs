// Input patch matrix: rows = sources (discovered, configured, manual), columns = device input
// pairs. A cell sends the source to that pair (click again to release); the headphone button
// previews the source; ✕ removes a manual or unadvertised row. Same behavior as the macOS grid.
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media;
using OpenLW.Daemon;

namespace OpenLW.Controls;

public sealed record GridRow(DiscoveredSource Source, int? PatchedColumn, string Origin, bool Removable);

public sealed class InputGrid : Grid
{
    private const double ColumnWidth = 96;
    private List<GridRow> rows = [];
    private List<List<int>> pairs = [];
    private int? listeningRow;
    private readonly List<Meter> columnMeters = [];
    private readonly List<TextBlock> columnStatus = [];
    private Meter? listenMeter;

    public event Action<int, int>? Toggle;   // row, column
    public event Action<int>? Listen;        // row
    public event Action<int>? Remove;        // row

    public InputGrid()
    {
        ColumnSpacing = 6;
        RowSpacing = 4;
    }

    /// Replaces rows, pairs and preview row; rebuilds only when something changed.
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

    public void ShowLevels(List<IReadOnlyList<double?>> columns, List<string> status, double? listenLevel)
    {
        for (int i = 0; i < columnMeters.Count && i < columns.Count; i++)
        {
            columnMeters[i].Show(columns[i]);
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
        // Columns: preview, channel, name, origin, one per pair, removal.
        foreach (double w in new double[] { 36, 60, 150, 170 })
        {
            ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(w) });
        }
        foreach (var _ in pairs)
        {
            ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(ColumnWidth) });
        }
        ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(32) });

        RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        for (int c = 0; c < pairs.Count; c++)
        {
            List<int> pair = pairs[c];
            var head = new StackPanel { Spacing = 3, HorizontalAlignment = HorizontalAlignment.Center };
            head.Children.Add(Caption($"Inputs {pair[0]}-{pair[^1]}", center: true));
            var meter = new Meter(2, ColumnWidth - 16);
            columnMeters.Add(meter);
            head.Children.Add(meter);
            var st = Caption("free", center: true);
            columnStatus.Add(st);
            head.Children.Add(st);
            Place(head, 0, 4 + c);
        }
        if (rows.Count == 0)
        {
            RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            var empty = Caption("No sources discovered. Choose the interface, or enter a channel below.");
            Place(empty, 1, 0);
            SetColumnSpan(empty, 4 + pairs.Count);
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
            ToolTipService.SetToolTip(listen, listeningRow == r ? "Stop listening" : "Listen on the Windows output");
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
            string kind = g.Source.PatchKind == "surround" ? " · surround" : g.Source.PatchKind == "backfeed" ? " · backfeed" : "";
            Place(Caption(g.Origin + kind), y, 3);

            for (int c = 0; c < pairs.Count; c++)
            {
                int column = c;
                bool on = g.PatchedColumn == c;
                var cell = new ToggleButton
                {
                    IsChecked = on,
                    Content = on ? new FontIcon { Glyph = "", FontSize = 12 } : null, // check mark
                    HorizontalAlignment = HorizontalAlignment.Stretch,
                    Height = 30,
                };
                ToolTipService.SetToolTip(cell, on
                    ? $"Release inputs {pairs[c][0]}-{pairs[c][^1]}"
                    : $"Send channel {g.Source.Channel} to inputs {pairs[c][0]}-{pairs[c][^1]}");
                cell.Click += (_, _) => Toggle?.Invoke(row, column);
                Place(cell, y, 4 + c);
            }
            if (g.Removable)
            {
                var remove = new Button { Content = "✕", Padding = new Thickness(6, 2, 6, 2) };
                ToolTipService.SetToolTip(remove, "Remove from grid");
                remove.Click += (_, _) => Remove?.Invoke(row);
                Place(remove, y, 4 + pairs.Count);
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

    private void Place(FrameworkElement e, int row, int column)
    {
        SetRow(e, row);
        SetColumn(e, column);
        Children.Add(e);
    }
}
