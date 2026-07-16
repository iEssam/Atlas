using Atlas.App.ViewModels;
using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Navigation;
using Microsoft.UI.Xaml.Shapes;
using Windows.Foundation;

namespace Atlas.App.Views;

/// <summary>The Evidence Atlas overview and fifteen-minute system trace.</summary>
public sealed partial class OverviewPage : Page
{
    public OverviewViewModel ViewModel { get; }
    public string CommitLine => $"Commit {ViewModel.CommitText}";

    public OverviewPage()
    {
        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        ViewModel = new OverviewViewModel(DispatcherQueue, string.IsNullOrEmpty(who) ? null : who);
        InitializeComponent();

        ViewModel.PropertyChanged += (_, e) =>
        {
            if (e.PropertyName is nameof(ViewModel.CommitText))
            {
                DispatcherQueue.TryEnqueue(() => Bindings.Update());
            }
        };
        ViewModel.TraceRefreshed += () => DispatcherQueue.TryEnqueue(RenderTrace);
        ActualThemeChanged += (_, _) => RenderTrace();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        ViewModel.Start();
    }

    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        base.OnNavigatedFrom(e);
        ViewModel.Stop();
    }

    private void RefreshTrace_Click(object sender, RoutedEventArgs e) => _ = ViewModel.RefreshTraceAsync();
    private void TraceCanvas_SizeChanged(object sender, SizeChangedEventArgs e) => RenderTrace();

    private void Evidence_ItemClick(object sender, ItemClickEventArgs e)
    {
        if (e.ClickedItem is OverviewEvidenceMarker marker && App.MainWindow is MainWindow window)
        {
            window.NavigateToEvidence(marker.Kind);
        }
    }

    private void RenderTrace()
    {
        TraceCanvas.Children.Clear();
        var width = TraceCanvas.ActualWidth;
        var height = TraceCanvas.ActualHeight;
        var span = ViewModel.TraceToMs - ViewModel.TraceFromMs;
        if (width <= 0 || height <= 0 || span <= 0 || ViewModel.IsTraceUnavailable)
        {
            return;
        }

        DrawReferenceLines(width, height);
        DrawSeries(ViewModel.CpuTrace, GetBrush("AtlasCyanBrush", Colors.Teal), width, height, span);
        DrawSeries(ViewModel.MemoryTrace, GetBrush("AtlasGreenBrush", Colors.ForestGreen), width, height, span);
        DrawSeries(ViewModel.GpuTrace, GetBrush("AtlasAmberBrush", Colors.Goldenrod), width, height, span);
        DrawEvidenceTicks(width, height, span);
    }

    private void DrawReferenceLines(double width, double height)
    {
        var brush = GetBrush("AtlasLineBrush", Colors.Gray);
        foreach (var percent in new[] { 25, 50, 75 })
        {
            var y = height - percent / 100.0 * height;
            TraceCanvas.Children.Add(new Line
            {
                X1 = 0,
                X2 = width,
                Y1 = y,
                Y2 = y,
                Stroke = brush,
                StrokeThickness = 1,
            });
        }
    }

    private void DrawSeries(
        IEnumerable<OverviewTracePoint> source,
        Brush brush,
        double width,
        double height,
        double span)
    {
        var points = new PointCollection();
        foreach (var point in source)
        {
            var x = (point.TimestampMs - ViewModel.TraceFromMs) / span * width;
            var y = height - Math.Clamp(point.Percent / 100.0, 0, 1) * height;
            points.Add(new Point(x, y));
        }
        if (points.Count >= 2)
        {
            TraceCanvas.Children.Add(new Polyline
            {
                Points = points,
                Stroke = brush,
                StrokeThickness = 1.75,
            });
        }
    }

    private void DrawEvidenceTicks(double width, double height, double span)
    {
        var brush = GetBrush("TextFillColorSecondaryBrush", Colors.Gray);
        foreach (var marker in ViewModel.Evidence)
        {
            var x = (marker.TimestampMs - ViewModel.TraceFromMs) / span * width;
            if (x < 0 || x > width)
            {
                continue;
            }

            var tick = new Line
            {
                X1 = x,
                X2 = x,
                Y1 = height - 11,
                Y2 = height,
                Stroke = brush,
                StrokeThickness = 2,
            };
            ToolTipService.SetToolTip(tick, $"{marker.Kind}: {marker.Summary}");
            TraceCanvas.Children.Add(tick);
        }
    }

    private static Brush GetBrush(string key, Windows.UI.Color fallback)
    {
        return Application.Current.Resources.TryGetValue(key, out var value) && value is Brush brush
            ? brush
            : new SolidColorBrush(fallback);
    }
}
