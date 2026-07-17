using System.ComponentModel;
using Atlas.App.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Navigation;
using Microsoft.UI.Xaml.Shapes;

namespace Atlas.App.Views;

/// <summary>
/// Stable graphics-process watchboard with adapter and process traces,
/// persistent application tracking, and a measured change feed.
/// </summary>
public sealed partial class GpuPage : Page
{
    private const double CompactTableBreakpoint = 900;
    private const double WideDetailBreakpoint = 1180;

    private GpuAdapterItem? _observedAdapter;
    private GpuProcessItem? _observedProcess;
    private bool _isCompact;
    private bool _isInitialized;
    private bool _isNarrow;
    private bool _isWide;

    public GpuViewModel ViewModel { get; }

    public GpuPage()
    {
        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        ViewModel = new GpuViewModel(
            DispatcherQueue,
            App.Preferences,
            string.IsNullOrEmpty(who) ? null : who);

        InitializeComponent();
        GpuSelector.SelectedItem = AllProcessesSelector;
        AdapterTraceMetricPicker.SelectedIndex = 0;
        ProcessTraceMetricPicker.SelectedIndex = 0;
        _isInitialized = true;
        ViewModel.PropertyChanged += ViewModel_PropertyChanged;
        UpdateViewState();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        ViewModel.Start();
    }

    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        ObserveAdapter(null);
        ObserveProcess(null);
        ViewModel.Stop();
        base.OnNavigatedFrom(e);
    }

    private void ViewModel_PropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName is nameof(ViewModel.SelectedAdapter))
        {
            ObserveAdapter(ViewModel.SelectedAdapter);
        }

        if (e.PropertyName is nameof(ViewModel.SelectedProcess))
        {
            ObserveProcess(ViewModel.SelectedProcess);
        }

        if (e.PropertyName is nameof(ViewModel.SelectedAdapter)
            or nameof(ViewModel.SelectedProcess)
            or nameof(ViewModel.HasSnapshot)
            or nameof(ViewModel.HasAdapter)
            or nameof(ViewModel.HasVisibleProcesses)
            or nameof(ViewModel.HasChanges)
            or nameof(ViewModel.IsUnavailable)
            or nameof(ViewModel.ViewMode))
        {
            UpdateViewState();
        }
    }

    private void Page_SizeChanged(object sender, SizeChangedEventArgs e)
    {
        if (!_isInitialized)
        {
            return;
        }

        _isCompact = e.NewSize.Width < CompactTableBreakpoint;
        _isNarrow = e.NewSize.Width < 720;
        _isWide = e.NewSize.Width >= WideDetailBreakpoint;
        UpdateViewState();
    }

    private void GpuSelector_SelectionChanged(
        SelectorBar sender,
        SelectorBarSelectionChangedEventArgs args)
    {
        if (!_isInitialized)
        {
            return;
        }

        var mode = sender.SelectedItem == TrackedSelector
            ? GpuViewMode.Tracked
            : sender.SelectedItem == ChangesSelector
                ? GpuViewMode.Changes
                : GpuViewMode.All;
        ViewModel.SetViewMode(mode);
        UpdateViewState();
    }

    private void ProcessSearch_TextChanged(AutoSuggestBox sender, AutoSuggestBoxTextChangedEventArgs args) =>
        ViewModel.SetSearchText(sender.Text);

    private void SortHeader_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: string tag }
            && Enum.TryParse<GpuProcessSortMode>(tag, out var mode))
        {
            ViewModel.SortBy(mode);
        }
    }

    private void PauseButton_Click(object sender, RoutedEventArgs e) => ViewModel.TogglePause();

    private async void TrackSelected_Click(object sender, RoutedEventArgs e) =>
        await ViewModel.ToggleSelectedTrackingAsync();

    private async void ProcessTrackButton_Click(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { DataContext: GpuProcessItem row })
        {
            await ViewModel.ToggleTrackingAsync(row);
        }
    }

    private async void ProcessTrack_Click(object sender, RoutedEventArgs e)
    {
        if (sender is MenuFlyoutItem { DataContext: GpuProcessItem row })
        {
            await ViewModel.ToggleTrackingAsync(row);
        }
    }

    private void InteractionInfoBar_Closed(InfoBar sender, InfoBarClosedEventArgs args) =>
        ViewModel.DismissInteractionMessage();

    private void ProcessList_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_isInitialized && sender is ListView { SelectedItem: GpuProcessItem process })
        {
            ViewModel.SelectedProcess = process;
        }
    }

    private void AdapterTraceMetricPicker_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_isInitialized)
        {
            RenderAdapterTrace();
        }
    }

    private void AdapterTraceCanvas_SizeChanged(object sender, SizeChangedEventArgs e)
    {
        if (_isInitialized)
        {
            RenderAdapterTrace();
        }
    }

    private void ProcessTraceMetricPicker_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_isInitialized)
        {
            RenderProcessTrace();
        }
    }

    private void ProcessTraceCanvas_SizeChanged(object sender, SizeChangedEventArgs e)
    {
        if (_isInitialized)
        {
            RenderProcessTrace();
        }
    }

    private void InspectSelected_Click(object sender, RoutedEventArgs e)
    {
        if (ViewModel.SelectedProcess is { IsRunning: true } selected)
        {
            OpenInspector(selected);
        }
    }

    private void ProcessInspect_Click(object sender, RoutedEventArgs e)
    {
        if (sender is MenuFlyoutItem { DataContext: GpuProcessItem { IsRunning: true } process })
        {
            OpenInspector(process);
        }
    }

    private void ProcessRow_DoubleTapped(
        object sender,
        Microsoft.UI.Xaml.Input.DoubleTappedRoutedEventArgs e)
    {
        if (sender is FrameworkElement { DataContext: GpuProcessItem { IsRunning: true } process })
        {
            OpenInspector(process);
        }
    }

    private void ObserveAdapter(GpuAdapterItem? adapter)
    {
        if (ReferenceEquals(_observedAdapter, adapter))
        {
            RenderAdapterTrace();
            return;
        }

        if (_observedAdapter is not null)
        {
            _observedAdapter.SamplesChanged -= ObservedAdapter_SamplesChanged;
        }

        _observedAdapter = adapter;
        if (_observedAdapter is not null)
        {
            _observedAdapter.SamplesChanged += ObservedAdapter_SamplesChanged;
        }
        RenderAdapterTrace();
    }

    private void ObservedAdapter_SamplesChanged(GpuAdapterItem adapter) => RenderAdapterTrace();

    private void ObserveProcess(GpuProcessItem? process)
    {
        if (ReferenceEquals(_observedProcess, process))
        {
            UpdateViewState();
            return;
        }

        if (_observedProcess is not null)
        {
            _observedProcess.SamplesChanged -= ObservedProcess_SamplesChanged;
        }

        _observedProcess = process;
        DetailsPane.DataContext = process;
        if (_observedProcess is not null)
        {
            _observedProcess.SamplesChanged += ObservedProcess_SamplesChanged;
        }
        RenderProcessTrace();
        UpdateViewState();
    }

    private void ObservedProcess_SamplesChanged(GpuProcessItem process) => RenderProcessTrace();

    private void UpdateViewState()
    {
        if (!_isInitialized)
        {
            return;
        }

        bool changes = ViewModel.ViewMode == GpuViewMode.Changes;
        bool hasAdapter = ViewModel.HasAdapter;
        bool showContent = ViewModel.HasSnapshot && hasAdapter;

        LoadingState.Visibility = ViewModel.HasSnapshot ? Visibility.Collapsed : Visibility.Visible;
        UnavailableState.Visibility = ViewModel.HasSnapshot && !hasAdapter
            ? Visibility.Visible
            : Visibility.Collapsed;
        InsightBand.Visibility = showContent ? Visibility.Visible : Visibility.Collapsed;
        TraceBand.Visibility = showContent && !changes && !_isNarrow
            ? Visibility.Visible
            : Visibility.Collapsed;
        ProcessSearch.IsEnabled = !changes;
        ProcessHost.Visibility = showContent && !changes ? Visibility.Visible : Visibility.Collapsed;
        ChangesHost.Visibility = showContent && changes ? Visibility.Visible : Visibility.Collapsed;
        ProcessEmptyState.Visibility = showContent && !changes && !ViewModel.HasVisibleProcesses
            ? Visibility.Visible
            : Visibility.Collapsed;
        InitialSnapshotProgress.IsActive = !ViewModel.HasSnapshot;
        InitialSnapshotProgress.Visibility = !ViewModel.HasSnapshot
            ? Visibility.Visible
            : Visibility.Collapsed;
        ChangesEmptyState.Visibility = changes && !ViewModel.HasChanges
            ? Visibility.Visible
            : Visibility.Collapsed;

        WideProcessHeader.Visibility = _isCompact ? Visibility.Collapsed : Visibility.Visible;
        WideProcessList.Visibility = _isCompact ? Visibility.Collapsed : Visibility.Visible;
        CompactProcessHeader.Visibility = _isCompact ? Visibility.Visible : Visibility.Collapsed;
        CompactProcessList.Visibility = _isCompact ? Visibility.Visible : Visibility.Collapsed;
        EngineColumn.Width = _isCompact ? new GridLength(0) : new GridLength(320);
        EnginePanel.Visibility = _isCompact ? Visibility.Collapsed : Visibility.Visible;
        VisibleCountLabel.Visibility = _isNarrow ? Visibility.Collapsed : Visibility.Visible;
        ApplyInsightLayout();

        bool showDetails = showContent
            && !changes
            && _isWide
            && ViewModel.SelectedProcess is not null;
        DetailsColumn.Width = showDetails ? new GridLength(380) : new GridLength(0);
        DetailsPane.Visibility = showDetails ? Visibility.Visible : Visibility.Collapsed;
    }

    private void ApplyInsightLayout()
    {
        if (_isNarrow)
        {
            InsightColumn0.Width = new GridLength(1, GridUnitType.Star);
            InsightColumn1.Width = new GridLength(1, GridUnitType.Star);
            InsightColumn2.Width = new GridLength(0);
            InsightColumn3.Width = new GridLength(0);
            Grid.SetRow(GpuMetric, 0);
            Grid.SetColumn(GpuMetric, 0);
            Grid.SetRow(DedicatedMetric, 0);
            Grid.SetColumn(DedicatedMetric, 1);
            Grid.SetRow(TemperatureMetric, 1);
            Grid.SetColumn(TemperatureMetric, 0);
            Grid.SetRow(TopProcessMetric, 1);
            Grid.SetColumn(TopProcessMetric, 1);
            return;
        }

        InsightColumn0.Width = new GridLength(1, GridUnitType.Star);
        InsightColumn1.Width = new GridLength(1.35, GridUnitType.Star);
        InsightColumn2.Width = new GridLength(1, GridUnitType.Star);
        InsightColumn3.Width = new GridLength(1.35, GridUnitType.Star);
        Grid.SetRow(GpuMetric, 0);
        Grid.SetColumn(GpuMetric, 0);
        Grid.SetRow(DedicatedMetric, 0);
        Grid.SetColumn(DedicatedMetric, 1);
        Grid.SetRow(TemperatureMetric, 0);
        Grid.SetColumn(TemperatureMetric, 2);
        Grid.SetRow(TopProcessMetric, 0);
        Grid.SetColumn(TopProcessMetric, 3);
    }

    private void RenderAdapterTrace()
    {
        if (!_isInitialized)
        {
            return;
        }

        AdapterTraceCanvas.Children.Clear();
        var samples = _observedAdapter?.GetSamples();
        if (samples is null || samples.Count == 0)
        {
            ClearAdapterTrace();
            return;
        }

        int metricIndex = Math.Max(0, AdapterTraceMetricPicker.SelectedIndex);
        Func<GpuAdapterLiveSample, double?> selector = metricIndex switch
        {
            1 => sample => sample.DedicatedUsedMb,
            2 => sample => sample.TemperatureC,
            3 => sample => sample.PowerW,
            _ => sample => sample.UtilizationPercent,
        };
        string metricName = metricIndex switch
        {
            1 => "dedicated memory",
            2 => "temperature",
            3 => "power draw",
            _ => "GPU utilization",
        };
        string unit = metricIndex switch
        {
            1 => " MB",
            2 => " °C",
            3 => " W",
            _ => " %",
        };

        var points = LastMinute(samples, sample => sample.TimestampMs, selector);
        RenderTrace(
            AdapterTraceCanvas,
            AdapterTraceEmptyText,
            AdapterTraceCurrentText,
            AdapterTraceAverageText,
            AdapterTracePeakText,
            points,
            unit,
            metricIndex == 0,
            $"{_observedAdapter!.Name} {metricName}");
    }

    private void RenderProcessTrace()
    {
        if (!_isInitialized)
        {
            return;
        }

        ProcessTraceCanvas.Children.Clear();
        var samples = _observedProcess?.GetSamples();
        if (samples is null || samples.Count == 0)
        {
            ClearProcessTrace();
            return;
        }

        int metricIndex = Math.Max(0, ProcessTraceMetricPicker.SelectedIndex);
        Func<GpuProcessLiveSample, double?> selector = metricIndex switch
        {
            1 => sample => sample.DedicatedMb,
            2 => sample => sample.SharedMb,
            _ => sample => sample.GpuPercent,
        };
        string metricName = metricIndex switch
        {
            1 => "dedicated memory",
            2 => "shared memory",
            _ => "GPU utilization",
        };
        string unit = metricIndex == 0 ? " %" : " MB";

        var points = LastMinute(samples, sample => sample.TimestampMs, selector);
        RenderTrace(
            ProcessTraceCanvas,
            ProcessTraceEmptyText,
            ProcessTraceCurrentText,
            ProcessTraceAverageText,
            ProcessTracePeakText,
            points,
            unit,
            metricIndex == 0,
            $"{_observedProcess!.Name} {metricName}");
    }

    private static (long TimestampMs, double Value)[] LastMinute<T>(
        IReadOnlyList<T> samples,
        Func<T, long> timestamp,
        Func<T, double?> value)
    {
        long latest = timestamp(samples[^1]);
        long cutoff = latest - (long)TimeSpan.FromMinutes(1).TotalMilliseconds;
        return samples
            .Select(sample => (TimestampMs: timestamp(sample), Value: value(sample)))
            .Where(point => point.TimestampMs >= cutoff && point.Value is { } measured && double.IsFinite(measured))
            .Select(point => (point.TimestampMs, point.Value!.Value))
            .ToArray();
    }

    private static void RenderTrace(
        Canvas canvas,
        TextBlock emptyText,
        TextBlock currentText,
        TextBlock averageText,
        TextBlock peakText,
        IReadOnlyList<(long TimestampMs, double Value)> points,
        string unit,
        bool startAtZero,
        string accessibleName)
    {
        canvas.Children.Clear();
        if (points.Count == 0)
        {
            ClearTrace(emptyText, currentText, averageText, peakText);
            return;
        }

        double current = points[^1].Value;
        double average = points.Average(point => point.Value);
        double peak = points.Max(point => point.Value);
        currentText.Text = FormatTraceValue(current, unit);
        averageText.Text = FormatTraceValue(average, unit);
        peakText.Text = FormatTraceValue(peak, unit);
        AutomationProperties.SetName(
            canvas,
            $"{accessibleName} trace. Current {currentText.Text}, average {averageText.Text}, peak {peakText.Text}.");

        double width = canvas.ActualWidth;
        double height = canvas.ActualHeight;
        if (points.Count < 2 || width <= 1 || height <= 1)
        {
            emptyText.Text = "Collecting the first minute of samples";
            emptyText.Visibility = Visibility.Visible;
            return;
        }

        long fromMs = points[0].TimestampMs;
        long latestMs = points[^1].TimestampMs;
        long spanMs = Math.Max(1, latestMs - fromMs);
        double minimum = startAtZero ? 0 : points.Min(point => point.Value);
        double maximum = startAtZero
            ? Math.Max(10, Math.Ceiling(peak / 10) * 10)
            : peak;
        if (maximum - minimum < 0.01)
        {
            maximum = minimum + 1;
        }
        else if (!startAtZero)
        {
            double padding = (maximum - minimum) * 0.12;
            minimum = Math.Max(0, minimum - padding);
            maximum += padding;
        }

        var line = new Polyline
        {
            StrokeThickness = 2,
            StrokeLineJoin = PenLineJoin.Round,
        };
        if (Application.Current.Resources.TryGetValue("AtlasCyanBrush", out var accent)
            && accent is Brush accentBrush)
        {
            line.Stroke = accentBrush;
        }
        else if (Application.Current.Resources.TryGetValue("TextFillColorPrimaryBrush", out var fallback)
                 && fallback is Brush fallbackBrush)
        {
            line.Stroke = fallbackBrush;
        }
        else
        {
            return;
        }

        foreach (var point in points)
        {
            double x = (point.TimestampMs - fromMs) / (double)spanMs * width;
            double normalized = (point.Value - minimum) / (maximum - minimum);
            double y = height - Math.Clamp(normalized, 0, 1) * height;
            if (double.IsFinite(x) && double.IsFinite(y))
            {
                line.Points.Add(new Windows.Foundation.Point(x, y));
            }
        }

        if (line.Points.Count >= 2)
        {
            canvas.Children.Add(line);
            emptyText.Visibility = Visibility.Collapsed;
        }
        else
        {
            emptyText.Visibility = Visibility.Visible;
        }
    }

    private void ClearAdapterTrace() => ClearTrace(
        AdapterTraceEmptyText,
        AdapterTraceCurrentText,
        AdapterTraceAverageText,
        AdapterTracePeakText);

    private void ClearProcessTrace() => ClearTrace(
        ProcessTraceEmptyText,
        ProcessTraceCurrentText,
        ProcessTraceAverageText,
        ProcessTracePeakText);

    private static void ClearTrace(
        TextBlock emptyText,
        TextBlock currentText,
        TextBlock averageText,
        TextBlock peakText)
    {
        emptyText.Text = "Waiting for live samples";
        emptyText.Visibility = Visibility.Visible;
        currentText.Text = "—";
        averageText.Text = "—";
        peakText.Text = "—";
    }

    private static string FormatTraceValue(double value, string unit) =>
        value < 10 ? $"{value:F1}{unit}" : $"{value:F0}{unit}";

    private void OpenInspector(GpuProcessItem process)
    {
        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        var inspector = new InspectorWindow(
            string.IsNullOrEmpty(who) ? null : who,
            process.Pid,
            process.CreateTime100ns,
            process.Name);
        inspector.Activate();
    }
}
