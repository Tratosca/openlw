// Horizontal peak meter, one bar per channel: −60 to 0 dBFS, green / amber (−18) / red (−6),
// like the macOS app. Ballistics: instant rise, then a fall of 20 dB in 1.7 s (IEC 60268-10
// type I return), animated on each rendered frame while a bar is falling.
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
    private const double Floor = -60, FallDbPerSecond = 20 / 1.7;
    private readonly List<Rectangle> bars = [];
    private readonly double?[] shown, target;
    private TimeSpan? lastFrame;
    private bool animating;

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
        shown = new double?[channels];
        target = new double?[channels];
        Unloaded += (_, _) => StopAnimation();
    }

    /// Peak per channel in dBFS (null: silence): shown at once if higher, otherwise reached by
    /// the fall.
    public void Show(IReadOnlyList<double?> levels)
    {
        for (int i = 0; i < target.Length; i++)
        {
            target[i] = i < levels.Count ? levels[i] : null;
        }
        if (Step(0) && !animating)
        {
            animating = true;
            lastFrame = null;
            CompositionTarget.Rendering += OnRendering;
        }
        Draw();
    }

    private void OnRendering(object? sender, object e)
    {
        TimeSpan now = ((RenderingEventArgs)e).RenderingTime;
        double dt = lastFrame is { } t ? Math.Min((now - t).TotalSeconds, 0.1) : 0;
        lastFrame = now;
        bool falling = Step(dt);
        Draw();
        if (!falling)
        {
            StopAnimation();
        }
    }

    private void StopAnimation()
    {
        if (animating)
        {
            CompositionTarget.Rendering -= OnRendering;
            animating = false;
        }
    }

    /// Falls by `dt` seconds toward the targets; true while a bar is still above its target.
    private bool Step(double dt)
    {
        bool falling = false;
        for (int i = 0; i < shown.Length; i++)
        {
            double? fallen = shown[i] - FallDbPerSecond * dt;
            double? level = target[i] is null ? fallen : fallen is null ? target[i] : Math.Max(target[i]!.Value, fallen.Value);
            shown[i] = level > Floor ? level : null;
            falling |= shown[i] != target[i];
        }
        return falling;
    }

    private void Draw()
    {
        for (int i = 0; i < bars.Count; i++)
        {
            double? db = shown[i];
            double frac = db is null ? 0 : Math.Clamp((db.Value + 60) / 60, 0, 1);
            bars[i].Width = frac * Width;
            bars[i].Fill = db >= -6 ? Red : db >= -18 ? Amber : Green;
        }
    }
}
