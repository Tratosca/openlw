// Horizontal peak meter, one bar per channel: −60 to 0 dBFS, green / amber (−18) / red (−6),
// like the macOS app.
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using Windows.UI;

namespace OpenLW.Controls;

public sealed class Meter : Grid
{
    private static readonly SolidColorBrush Green = new(Color.FromArgb(255, 0x34, 0xC7, 0x59));
    private static readonly SolidColorBrush Amber = new(Color.FromArgb(255, 0xFF, 0x9F, 0x0A));
    private static readonly SolidColorBrush Red = new(Color.FromArgb(255, 0xFF, 0x3B, 0x30));
    private readonly List<Rectangle> bars = [];

    public Meter(int channels = 2, double width = 110)
    {
        Width = width;
        RowSpacing = 2;
        for (int i = 0; i < channels; i++)
        {
            RowDefinitions.Add(new RowDefinition { Height = new GridLength(4) });
            var track = new Rectangle
            {
                Fill = (Brush)Application.Current.Resources["ControlStrokeColorDefaultBrush"],
                RadiusX = 2, RadiusY = 2,
            };
            var bar = new Rectangle { HorizontalAlignment = HorizontalAlignment.Left, Width = 0, RadiusX = 2, RadiusY = 2, Fill = Green };
            SetRow(track, i);
            SetRow(bar, i);
            Children.Add(track);
            Children.Add(bar);
            bars.Add(bar);
        }
        VerticalAlignment = VerticalAlignment.Center;
    }

    /// Peak per channel in dBFS (null: silence).
    public void Show(IReadOnlyList<double?> levels)
    {
        for (int i = 0; i < bars.Count; i++)
        {
            double? db = i < levels.Count ? levels[i] : null;
            double frac = db is null ? 0 : Math.Clamp((db.Value + 60) / 60, 0, 1);
            bars[i].Width = frac * Width;
            bars[i].Fill = db >= -6 ? Red : db >= -18 ? Amber : Green;
        }
    }
}
