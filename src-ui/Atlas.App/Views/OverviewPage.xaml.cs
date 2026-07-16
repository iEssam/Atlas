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

    private void PageLayout_SizeChanged(object sender, SizeChangedEventArgs e)
    {
        // AdaptiveTrigger is retained in XAML as the layout contract. Apply the
        // same values explicitly because some unpackaged Windows App SDK hosts
        // do not reevaluate page-level adaptive triggers after Frame navigation.
        var large = e.NewSize.Width >= 1008;
        var medium = !large && e.NewSize.Width >= 640;

        PageLayout.Padding = large
            ? new Thickness(24)
            : medium
                ? new Thickness(24, 48, 24, 24)
                : new Thickness(16, 48, 16, 16);

        MetricColumn1.Width = medium || large ? new GridLength(1, GridUnitType.Star) : new GridLength(0);
        MetricColumn2.Width = large ? new GridLength(1, GridUnitType.Star) : new GridLength(0);
        MetricColumn3.Width = large ? new GridLength(1, GridUnitType.Star) : new GridLength(0);

        Grid.SetRow(GraphicsMetric, medium || large ? 0 : 1);
        Grid.SetColumn(GraphicsMetric, medium || large ? 1 : 0);
        Grid.SetRow(MemoryMetric, large ? 0 : medium ? 1 : 2);
        Grid.SetColumn(MemoryMetric, large ? 2 : 0);
        Grid.SetRow(ProcessesMetric, large ? 0 : medium ? 1 : 3);
        Grid.SetColumn(ProcessesMetric, large ? 3 : medium ? 1 : 0);

        EvidenceColumn.Width = new GridLength(large ? 2 : 1, GridUnitType.Star);
        ConsumersColumn.Width = large ? new GridLength(3, GridUnitType.Star) : new GridLength(0);
        Grid.SetRow(ConsumersSection, large ? 0 : 1);
        Grid.SetColumn(ConsumersSection, large ? 1 : 0);
        WideConsumers.Visibility = large ? Visibility.Visible : Visibility.Collapsed;
        CompactConsumers.Visibility = large ? Visibility.Collapsed : Visibility.Visible;
    }

    private void EvidenceRow_Click(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { DataContext: OverviewEvidenceMarker marker } &&
            App.MainWindow is MainWindow window)
        {
            window.NavigateToEvidence(marker.Kind);
        }
    }

    private void ConsumerRow_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not FrameworkElement { DataContext: ConsumerRowViewModel row })
        {
            return;
        }

        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        var inspector = new InspectorWindow(
            string.IsNullOrEmpty(who) ? null : who,
            row.Pid,
            row.CreateTime100ns,
            row.ImageName);
        inspector.Activate();
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
        DrawSeries(
            ViewModel.CpuTrace,
            GetBrush("AtlasCyanBrush", Colors.Teal),
            GetBrush("AtlasCyanTintBrush", Colors.Transparent),
            null,
            width,
            height,
            span);
        DrawSeries(
            ViewModel.MemoryTrace,
            GetBrush("AtlasGreenBrush", Colors.ForestGreen),
            GetBrush("AtlasGreenTintBrush", Colors.Transparent),
            new DoubleCollection { 1, 3 },
            width,
            height,
            span);
        DrawSeries(
            ViewModel.GpuTrace,
            GetBrush("AtlasAmberBrush", Colors.Goldenrod),
            GetBrush("AtlasAmberTintBrush", Colors.Transparent),
            new DoubleCollection { 6, 4 },
            width,
            height,
            span);
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
        Brush lineBrush,
        Brush bandBrush,
        DoubleCollection? dashPattern,
        double width,
        double height,
        double span)
    {
        var points = source.OrderBy(point => point.TimestampMs).ToList();
        if (points.Count == 0)
        {
            return;
        }

        var expectedStep = span / 180.0;
        int segmentStart = 0;
        for (int index = 1; index <= points.Count; index++)
        {
            var endsSegment = index == points.Count ||
                points[index].TimestampMs - points[index - 1].TimestampMs > expectedStep * 1.75;
            if (!endsSegment)
            {
                continue;
            }

            DrawSeriesSegment(
                points,
                segmentStart,
                index - 1,
                lineBrush,
                bandBrush,
                dashPattern,
                width,
                height,
                span);
            segmentStart = index;
        }
    }

    private void DrawSeriesSegment(
        IReadOnlyList<OverviewTracePoint> points,
        int start,
        int end,
        Brush lineBrush,
        Brush bandBrush,
        DoubleCollection? dashPattern,
        double width,
        double height,
        double span)
    {
        double X(OverviewTracePoint point) =>
            (point.TimestampMs - ViewModel.TraceFromMs) / span * width;
        double Y(double percent) => height - Math.Clamp(percent / 100.0, 0, 1) * height;

        var band = new PointCollection();
        for (int index = start; index <= end; index++)
        {
            band.Add(new Point(X(points[index]), Y(points[index].MaxPercent)));
        }
        for (int index = end; index >= start; index--)
        {
            band.Add(new Point(X(points[index]), Y(points[index].MinPercent)));
        }
        if (band.Count >= 3)
        {
            TraceCanvas.Children.Add(new Polygon { Points = band, Fill = bandBrush });
        }

        var average = new PointCollection();
        for (int index = start; index <= end; index++)
        {
            average.Add(new Point(X(points[index]), Y(points[index].AveragePercent)));
        }

        if (average.Count == 1)
        {
            var dot = new Ellipse { Width = 4, Height = 4, Fill = lineBrush };
            Canvas.SetLeft(dot, average[0].X - 2);
            Canvas.SetTop(dot, average[0].Y - 2);
            TraceCanvas.Children.Add(dot);
            return;
        }

        var line = new Polyline
        {
            Points = average,
            Stroke = lineBrush,
            StrokeThickness = 1.75,
        };
        if (dashPattern is not null)
        {
            line.StrokeDashArray = dashPattern;
        }
        TraceCanvas.Children.Add(line);
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
