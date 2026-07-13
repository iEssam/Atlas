using System;
using System.Collections.Generic;
using Atlas.App.ViewModels;
using Atlas.IpcClient;
using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Navigation;
using Microsoft.UI.Xaml.Shapes;
using Windows.Foundation;
using Windows.UI;

namespace Atlas.App.Views;

/// <summary>
/// The Timeline page (M6): a hand-drawn system-CPU chart (min/max band + avg
/// line) over a selectable window, with a process-event lane and bookmark
/// markers. No external chart library — the band/line/markers are drawn onto a
/// <see cref="Canvas"/> with plain <see cref="Polyline"/>/<see cref="Polygon"/>
/// shapes (task brief §2). Data gaps render as breaks, never zeros (PRD §11.3).
/// </summary>
public sealed partial class TimelinePage : Page
{
    public TimelineViewModel ViewModel { get; }

    // Fixed 0..100% Y axis so the chart reads consistently across windows.
    private const double AxisMaxPercent = 100.0;

    public TimelinePage()
    {
        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        ViewModel = new TimelineViewModel(
            DispatcherQueue, string.IsNullOrEmpty(who) ? null : who);

        InitializeComponent();

        ViewModel.DataRefreshed += () => DispatcherQueue.TryEnqueue(Render);
    }

    protected override async void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        await ViewModel.RefreshAsync();
    }

    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        base.OnNavigatedFrom(e);
        ViewModel.Stop();
    }

    private void Refresh_Click(object sender, RoutedEventArgs e) => _ = ViewModel.RefreshAsync();

    private void ChartCanvas_SizeChanged(object sender, SizeChangedEventArgs e) => Render();

    private async void BookmarkNow_Click(object sender, RoutedEventArgs e)
    {
        var input = new TextBox
        {
            PlaceholderText = "Label (optional)",
            AcceptsReturn = false,
        };
        AutomationProperties.SetName(input, "Bookmark label");

        var dialog = new ContentDialog
        {
            Title = "Bookmark this moment",
            Content = input,
            PrimaryButtonText = "Add bookmark",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
            XamlRoot = XamlRoot,
        };

        if (await dialog.ShowAsync() == ContentDialogResult.Primary)
        {
            var status = await ViewModel.BookmarkNowAsync(input.Text);
            ViewModel.StatusText = status;
        }
    }

    /// <summary>
    /// Redraws the whole chart: min/max band polygon, avg polyline (broken
    /// across gaps), and bookmark vertical lines. Cheap enough to run on every
    /// refresh / resize since history is not live.
    /// </summary>
    private void Render()
    {
        var canvas = ChartCanvas;
        canvas.Children.Clear();

        double w = canvas.ActualWidth;
        double h = canvas.ActualHeight;
        if (w <= 0 || h <= 0 || ViewModel.IsUnavailable)
        {
            return;
        }

        double from = ViewModel.WindowFromMs;
        double to = ViewModel.WindowToMs;
        double spanMs = to - from;
        if (spanMs <= 0)
        {
            return;
        }

        DrawGridlines(canvas, w, h);

        var points = ViewModel.CpuPoints;
        if (points.Count == 0)
        {
            return;
        }

        double X(long tsMs) => (tsMs - from) / spanMs * w;
        double Y(double percent) => h - Math.Clamp(percent / AxisMaxPercent, 0, 1) * h;

        long step = ViewModel.BucketStepMs;
        var bandBrush = new SolidColorBrush(ColorFromAccent(0x33));
        var lineBrush = new SolidColorBrush(ColorFromAccent(0xFF));

        // Split into contiguous segments, breaking wherever there's a gap so a
        // missing stretch renders as an actual break (PRD §11.3), never zero.
        int i = 0;
        while (i < points.Count)
        {
            int j = i;
            while (j + 1 < points.Count &&
                   !HistoryFormatter.IsGap(points[j].StartMs, points[j + 1].StartMs, step))
            {
                j++;
            }

            DrawSegment(canvas, points, i, j, X, Y, bandBrush, lineBrush);
            i = j + 1;
        }

        DrawBookmarks(canvas, w, h, from, spanMs);
    }

    private void DrawSegment(
        Canvas canvas,
        System.Collections.ObjectModel.ObservableCollection<HistoryFormatter.TimelinePoint> points,
        int start, int end,
        Func<long, double> X, Func<double, double> Y,
        Brush bandBrush, Brush lineBrush)
    {
        // Band polygon: forward along max, back along min.
        var band = new PointCollection();
        for (int k = start; k <= end; k++)
        {
            band.Add(new Point(X(points[k].StartMs), Y(points[k].MaxPercent)));
        }
        for (int k = end; k >= start; k--)
        {
            band.Add(new Point(X(points[k].StartMs), Y(points[k].MinPercent)));
        }
        if (band.Count >= 3)
        {
            canvas.Children.Add(new Polygon { Points = band, Fill = bandBrush });
        }

        // Avg polyline.
        var avg = new PointCollection();
        for (int k = start; k <= end; k++)
        {
            avg.Add(new Point(X(points[k].StartMs), Y(points[k].AvgPercent)));
        }
        if (avg.Count == 1)
        {
            // A lone point can't form a line; draw a small dot so it's visible.
            var p = avg[0];
            canvas.Children.Add(new Ellipse
            {
                Width = 3, Height = 3, Fill = lineBrush,
            }.At(p.X - 1.5, p.Y - 1.5));
        }
        else if (avg.Count >= 2)
        {
            canvas.Children.Add(new Polyline
            {
                Points = avg, Stroke = lineBrush, StrokeThickness = 1.5,
            });
        }
    }

    private void DrawGridlines(Canvas canvas, double w, double h)
    {
        var gridBrush = new SolidColorBrush(Color.FromArgb(0x22, 0x80, 0x80, 0x80));
        for (int pct = 0; pct <= 100; pct += 25)
        {
            double y = h - pct / AxisMaxPercent * h;
            canvas.Children.Add(new Line
            {
                X1 = 0, X2 = w, Y1 = y, Y2 = y,
                Stroke = gridBrush, StrokeThickness = 1,
            });
            canvas.Children.Add(new TextBlock
            {
                Text = pct + "%",
                FontSize = 10,
                Opacity = 0.6,
            }.At(4, Math.Max(0, y - 14)));
        }
    }

    private void DrawBookmarks(Canvas canvas, double w, double h, double from, double spanMs)
    {
        var markBrush = new SolidColorBrush(Color.FromArgb(0xCC, 0xFF, 0xB9, 0x00)); // amber
        foreach (var b in ViewModel.Bookmarks)
        {
            double x = (b.TsMs - from) / spanMs * w;
            if (x < 0 || x > w)
            {
                continue;
            }
            var line = new Line
            {
                X1 = x, X2 = x, Y1 = 0, Y2 = h,
                Stroke = markBrush, StrokeThickness = 1.5,
            };
            ToolTipService.SetToolTip(line, string.IsNullOrEmpty(b.Label) ? "Bookmark" : b.Label);
            canvas.Children.Add(line);
        }
    }

    private static Color ColorFromAccent(byte alpha)
    {
        // Use the system accent so the chart matches the Fluent theme.
        if (Application.Current.Resources.TryGetValue("SystemAccentColor", out var v)
            && v is Color c)
        {
            return Color.FromArgb(alpha, c.R, c.G, c.B);
        }
        return Color.FromArgb(alpha, 0x00, 0x78, 0xD4); // default Windows blue
    }
}

/// <summary>Canvas-position fluent helper to keep the render code terse.</summary>
internal static class CanvasElementExtensions
{
    public static T At<T>(this T element, double x, double y) where T : UIElement
    {
        Canvas.SetLeft(element, x);
        Canvas.SetTop(element, y);
        return element;
    }
}
