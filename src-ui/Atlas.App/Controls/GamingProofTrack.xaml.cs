using Atlas.V0;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.Foundation;

namespace Atlas.App.Controls;

public sealed partial class GamingProofTrack : UserControl
{
    private IReadOnlyList<GamingTraceBucket> _buckets = Array.Empty<GamingTraceBucket>();

    public GamingProofTrack()
    {
        InitializeComponent();
        ActualThemeChanged += (_, _) => Render();
    }

    public static readonly DependencyProperty EmptyMessageProperty = DependencyProperty.Register(
        nameof(EmptyMessage),
        typeof(string),
        typeof(GamingProofTrack),
        new PropertyMetadata("Record or select a session to populate the synchronized trace."));

    public string EmptyMessage
    {
        get => (string)GetValue(EmptyMessageProperty);
        set => SetValue(EmptyMessageProperty, value);
    }

    public void SetTrace(IEnumerable<GamingTraceBucket> buckets, bool live = false)
    {
        _buckets = buckets.ToArray();
        EmptyText.Visibility = _buckets.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        TraceContent.Visibility = _buckets.Count == 0 ? Visibility.Collapsed : Visibility.Visible;
        LiveCursor.Visibility = live && _buckets.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
        FrameLiveCursor.Visibility = live && _buckets.Any(bucket => bucket.FrameTimeMs > 0)
            ? Visibility.Visible
            : Visibility.Collapsed;
        FrameStatusText.Text = _buckets.Any(bucket => bucket.FrameTimeMs > 0)
            ? "Frame time p95 by second"
            : "Frame time was not captured for this recording";
        Render();
    }

    private void PlotCanvas_SizeChanged(object sender, SizeChangedEventArgs e) => Render();

    private void Render()
    {
        CpuLine.Points.Clear();
        GpuLine.Points.Clear();
        MemoryLine.Points.Clear();
        FrameLine.Points.Clear();
        var width = PlotCanvas.ActualWidth;
        var height = PlotCanvas.ActualHeight;
        if (_buckets.Count == 0 || width <= 1 || height <= 1) return;

        var maxMemory = Math.Max(1UL, _buckets.Max(bucket => bucket.RamUsedBytes));
        var frameHeight = FrameCanvas.ActualHeight;
        var frameWidth = FrameCanvas.ActualWidth;
        var maxFrameTime = Math.Max(50.0, _buckets.Max(bucket => bucket.FrameTimeMs));
        for (var index = 0; index < _buckets.Count; index++)
        {
            var bucket = _buckets[index];
            var x = _buckets.Count == 1 ? width / 2 : width * index / (_buckets.Count - 1);
            CpuLine.Points.Add(new Point(x, ValueY(bucket.CpuPercent, height)));
            GpuLine.Points.Add(new Point(x, ValueY(bucket.GpuPercent, height)));
            MemoryLine.Points.Add(new Point(x, ValueY(bucket.RamUsedBytes * 100.0 / maxMemory, height)));
            if (bucket.FrameTimeMs > 0 && frameWidth > 1 && frameHeight > 1)
            {
                var frameX = _buckets.Count == 1 ? frameWidth / 2 : frameWidth * index / (_buckets.Count - 1);
                FrameLine.Points.Add(new Point(frameX, frameHeight - Math.Clamp(bucket.FrameTimeMs / maxFrameTime, 0, 1) * frameHeight));
            }
        }

        LiveCursor.X1 = width;
        LiveCursor.X2 = width;
        LiveCursor.Y1 = 0;
        LiveCursor.Y2 = height;
        FrameLiveCursor.X1 = frameWidth;
        FrameLiveCursor.X2 = frameWidth;
        FrameLiveCursor.Y1 = 0;
        FrameLiveCursor.Y2 = frameHeight;
    }

    private static double ValueY(double percent, double height) =>
        height - Math.Clamp(percent, 0, 100) / 100.0 * height;
}
