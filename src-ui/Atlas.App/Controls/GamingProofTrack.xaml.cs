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

    public void SetTrace(IEnumerable<GamingTraceBucket> buckets, bool live = false)
    {
        _buckets = buckets.ToArray();
        EmptyText.Visibility = _buckets.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        LiveCursor.Visibility = live && _buckets.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
        Render();
    }

    private void PlotCanvas_SizeChanged(object sender, SizeChangedEventArgs e) => Render();

    private void Render()
    {
        CpuLine.Points.Clear();
        GpuLine.Points.Clear();
        MemoryLine.Points.Clear();
        var width = PlotCanvas.ActualWidth;
        var height = PlotCanvas.ActualHeight;
        if (_buckets.Count == 0 || width <= 1 || height <= 1) return;

        var maxMemory = Math.Max(1UL, _buckets.Max(bucket => bucket.RamUsedBytes));
        for (var index = 0; index < _buckets.Count; index++)
        {
            var bucket = _buckets[index];
            var x = _buckets.Count == 1 ? width / 2 : width * index / (_buckets.Count - 1);
            CpuLine.Points.Add(new Point(x, ValueY(bucket.CpuPercent, height)));
            GpuLine.Points.Add(new Point(x, ValueY(bucket.GpuPercent, height)));
            MemoryLine.Points.Add(new Point(x, ValueY(bucket.RamUsedBytes * 100.0 / maxMemory, height)));
        }

        LiveCursor.X1 = width;
        LiveCursor.X2 = width;
        LiveCursor.Y1 = 0;
        LiveCursor.Y2 = height;
    }

    private static double ValueY(double percent, double height) =>
        height - Math.Clamp(percent, 0, 100) / 100.0 * height;
}
