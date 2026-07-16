using System.ComponentModel;
using Atlas.App.ViewModels;
using Atlas.IpcClient;
using Atlas.V0;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Navigation;
using Microsoft.UI.Xaml.Shapes;

namespace Atlas.App.Views;

/// <summary>
/// Stable live process table, tracked-application view, measured change feed,
/// and a wide-screen selected-process detail surface.
/// </summary>
public sealed partial class LiveActivityPage : Page
{
    private const double WideDetailBreakpoint = 1160;

    private ProcessRowViewModel? _observedProcess;
    private bool _isInitialized;
    private bool _isWide;

    public LiveActivityViewModel ViewModel { get; }

    public string CpuText => $"{ViewModel.SystemCpuPercent:F1} %";
    public string GpuText => $"{ViewModel.SystemGpuPercent:F1} %";
    public string MemText => $"{ViewModel.MemUsedGb:F1} / {ViewModel.MemTotalGb:F1} GB";

    public LiveActivityPage()
    {
        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        ViewModel = new LiveActivityViewModel(
            DispatcherQueue,
            App.Preferences,
            string.IsNullOrEmpty(who) ? null : who);

        InitializeComponent();
        ActivitySelector.SelectedItem = AllProcessesSelector;
        TraceMetricPicker.SelectedIndex = 0;
        _isInitialized = true;
        ViewModel.PropertyChanged += ViewModel_PropertyChanged;
        UpdateViewState();
    }

    public static Visibility EndedVisibility(bool isRunning) =>
        isRunning ? Visibility.Collapsed : Visibility.Visible;

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        ViewModel.Start();
    }

    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        ObserveProcess(null);
        ViewModel.Stop();
        base.OnNavigatedFrom(e);
    }

    private void ViewModel_PropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName is nameof(ViewModel.SystemCpuPercent)
            or nameof(ViewModel.SystemGpuPercent)
            or nameof(ViewModel.MemUsedGb)
            or nameof(ViewModel.MemTotalGb))
        {
            Bindings.Update();
        }

        if (e.PropertyName is nameof(ViewModel.SelectedProcess))
        {
            ObserveProcess(ViewModel.SelectedProcess);
        }

        if (e.PropertyName is nameof(ViewModel.SelectedProcess)
            or nameof(ViewModel.HasVisibleProcesses)
            or nameof(ViewModel.HasChanges)
            or nameof(ViewModel.HasSnapshot))
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

        _isWide = e.NewSize.Width >= WideDetailBreakpoint;
        UpdateViewState();
    }

    private void ActivitySelector_SelectionChanged(
        SelectorBar sender,
        SelectorBarSelectionChangedEventArgs args)
    {
        if (!_isInitialized)
        {
            return;
        }

        var mode = sender.SelectedItem == TrackedSelector
            ? ActivityViewMode.Tracked
            : sender.SelectedItem == ChangesSelector
                ? ActivityViewMode.Changes
                : ActivityViewMode.All;
        ViewModel.SetViewMode(mode);
        UpdateViewState();
    }

    private void ProcessSearch_TextChanged(AutoSuggestBox sender, AutoSuggestBoxTextChangedEventArgs args) =>
        ViewModel.SetSearchText(sender.Text);

    private void SortHeader_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: string tag }
            && Enum.TryParse<ProcessSortMode>(tag, out var mode))
        {
            ViewModel.SortBy(mode);
        }
    }

    private void PauseButton_Click(object sender, RoutedEventArgs e) => ViewModel.TogglePause();

    private async void TrackSelected_Click(object sender, RoutedEventArgs e) =>
        await ViewModel.ToggleSelectedTrackingAsync();

    private async void ProcessTrackButton_Click(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { DataContext: ProcessRowViewModel row })
        {
            await ViewModel.ToggleTrackingAsync(row);
        }
    }

    private async void ProcessTrack_Click(object sender, RoutedEventArgs e)
    {
        if (sender is MenuFlyoutItem { DataContext: ProcessRowViewModel row })
        {
            await ViewModel.ToggleTrackingAsync(row);
        }
    }

    private void InteractionInfoBar_Closed(InfoBar sender, InfoBarClosedEventArgs args) =>
        ViewModel.DismissInteractionMessage();

    private void ProcessList_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_isInitialized)
        {
            ObserveProcess(ViewModel.SelectedProcess);
        }
    }

    private void TraceMetricPicker_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_isInitialized)
        {
            RenderSelectedTrace();
        }
    }

    private void SelectedTraceCanvas_SizeChanged(object sender, SizeChangedEventArgs e)
    {
        if (_isInitialized)
        {
            RenderSelectedTrace();
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
        if (sender is MenuFlyoutItem { DataContext: ProcessRowViewModel { IsRunning: true } row })
        {
            OpenInspector(row);
        }
    }

    private void ProcessRow_DoubleTapped(
        object sender,
        Microsoft.UI.Xaml.Input.DoubleTappedRoutedEventArgs e)
    {
        if (sender is FrameworkElement { DataContext: ProcessRowViewModel { IsRunning: true } row })
        {
            OpenInspector(row);
        }
    }

    private void ObserveProcess(ProcessRowViewModel? process)
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

        RenderSelectedTrace();
        UpdateViewState();
    }

    private void ObservedProcess_SamplesChanged(ProcessRowViewModel process) => RenderSelectedTrace();

    private void UpdateViewState()
    {
        bool changes = ViewModel.ViewMode == ActivityViewMode.Changes;
        ProcessSearch.IsEnabled = !changes;
        ProcessListHost.Visibility = changes ? Visibility.Collapsed : Visibility.Visible;
        ChangesHost.Visibility = changes ? Visibility.Visible : Visibility.Collapsed;
        ProcessEmptyState.Visibility = !changes && !ViewModel.HasVisibleProcesses
            ? Visibility.Visible
            : Visibility.Collapsed;
        InitialSnapshotProgress.IsActive = !ViewModel.HasSnapshot;
        InitialSnapshotProgress.Visibility = !ViewModel.HasSnapshot
            ? Visibility.Visible
            : Visibility.Collapsed;
        ChangesEmptyState.Visibility = changes && !ViewModel.HasChanges
            ? Visibility.Visible
            : Visibility.Collapsed;

        bool showDetails = !changes && _isWide && ViewModel.SelectedProcess is not null;
        DetailsColumn.Width = showDetails ? new GridLength(360) : new GridLength(0);
        DetailsPane.Visibility = showDetails ? Visibility.Visible : Visibility.Collapsed;
    }

    private void RenderSelectedTrace()
    {
        SelectedTraceCanvas.Children.Clear();
        if (_observedProcess is null)
        {
            ClearTraceSummary();
            return;
        }

        var samples = _observedProcess.GetSamples();
        if (samples.Count == 0)
        {
            ClearTraceSummary();
            return;
        }

        int metricIndex = Math.Max(0, TraceMetricPicker.SelectedIndex);
        Func<ProcessLiveSample, double> selector = metricIndex switch
        {
            1 => sample => sample.WorkingSetMb,
            2 => sample => sample.GpuPercent,
            3 => sample => sample.DiskMbPerSecond,
            _ => sample => sample.CpuPercent,
        };
        string metricName = metricIndex switch
        {
            1 => "memory",
            2 => "GPU",
            3 => "disk activity",
            _ => "CPU",
        };
        string unit = metricIndex switch
        {
            1 => " MB",
            3 => " MB/s",
            _ => " %",
        };

        long latestMs = samples[^1].TimestampMs;
        long cutoff = latestMs - (long)TimeSpan.FromMinutes(1).TotalMilliseconds;
        var recent = samples
            .Where(sample => sample.TimestampMs >= cutoff)
            .Select(sample => (sample.TimestampMs, Value: selector(sample)))
            .Where(point => double.IsFinite(point.Value))
            .ToArray();

        if (recent.Length == 0)
        {
            ClearTraceSummary();
            return;
        }

        double current = recent[^1].Value;
        double average = recent.Average(point => point.Value);
        double peak = recent.Max(point => point.Value);
        TraceCurrentText.Text = FormatTraceValue(current, unit);
        TraceAverageText.Text = FormatTraceValue(average, unit);
        TracePeakText.Text = FormatTraceValue(peak, unit);
        AutomationProperties.SetName(
            SelectedTraceCanvas,
            $"{_observedProcess.ImageName} {metricName} trace. Current {TraceCurrentText.Text}, average {TraceAverageText.Text}, peak {TracePeakText.Text}.");

        double width = SelectedTraceCanvas.ActualWidth;
        double height = SelectedTraceCanvas.ActualHeight;
        if (recent.Length < 2 || width <= 1 || height <= 1)
        {
            TraceEmptyText.Visibility = Visibility.Visible;
            TraceEmptyText.Text = "Collecting the first minute of samples";
            return;
        }

        long fromMs = recent[0].TimestampMs;
        long spanMs = Math.Max(1, latestMs - fromMs);
        double minimum = metricIndex is 0 or 2 ? 0 : recent.Min(point => point.Value);
        double maximum = metricIndex is 0 or 2
            ? Math.Max(10, Math.Ceiling(peak / 10) * 10)
            : peak;
        if (maximum - minimum < 0.01)
        {
            maximum = minimum + 1;
        }
        else if (metricIndex is 1 or 3)
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
        if (Application.Current.Resources.TryGetValue("AtlasCyanBrush", out var brush)
            && brush is Brush traceBrush)
        {
            line.Stroke = traceBrush;
        }
        else
        {
            if (Application.Current.Resources.TryGetValue("TextFillColorPrimaryBrush", out var fallback)
                && fallback is Brush fallbackBrush)
            {
                line.Stroke = fallbackBrush;
            }
            else
            {
                return;
            }
        }

        foreach (var point in recent)
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
            SelectedTraceCanvas.Children.Add(line);
            TraceEmptyText.Visibility = Visibility.Collapsed;
        }
        else
        {
            TraceEmptyText.Visibility = Visibility.Visible;
        }
    }

    private void ClearTraceSummary()
    {
        TraceEmptyText.Visibility = Visibility.Visible;
        TraceEmptyText.Text = "Waiting for live samples";
        TraceCurrentText.Text = "\u2014";
        TraceAverageText.Text = "\u2014";
        TracePeakText.Text = "\u2014";
    }

    private static string FormatTraceValue(double value, string unit) =>
        value < 10 ? $"{value:F1}{unit}" : $"{value:F0}{unit}";

    private void OpenInspector(ProcessRowViewModel row)
    {
        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        var inspector = new InspectorWindow(
            string.IsNullOrEmpty(who) ? null : who,
            row.Pid,
            row.CreateTime100ns,
            row.ImageName);
        inspector.Activate();
    }

    private async void ProcessAction_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not MenuFlyoutItem item
            || item.DataContext is not ProcessRowViewModel { IsRunning: true } row)
        {
            return;
        }

        var action = (item.Tag as string) switch
        {
            "close" => ProcessActionKind.CloseWindows,
            "suspend" => ProcessActionKind.Suspend,
            "resume" => ProcessActionKind.Resume,
            "terminate" => ProcessActionKind.Terminate,
            _ => ProcessActionKind.ProcessActionUnspecified,
        };
        if (action == ProcessActionKind.ProcessActionUnspecified)
        {
            return;
        }

        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        var useFake = Environment.GetEnvironmentVariable("ATLAS_FAKE_BROKER") == "1";

        IActionBroker broker;
        AtlasChannel? channel = null;
        if (useFake)
        {
            broker = BuildDemoBroker(action);
        }
        else
        {
            channel = AtlasChannel.Connect(string.IsNullOrEmpty(who) ? null : who);
            broker = new ChannelActionBroker(channel);
        }

        try
        {
            var dialog = new SafeActionDialog(
                broker,
                row.Pid,
                row.CreateTime100ns,
                action,
                $"{row.ImageName} (pid {row.Pid})")
            {
                XamlRoot = XamlRoot,
            };
            await dialog.ShowAsync();
        }
        finally
        {
            channel?.Dispose();
        }
    }

    private static IActionBroker BuildDemoBroker(ProcessActionKind action)
    {
        if (action == ProcessActionKind.Terminate)
        {
            return FakeActionBroker.Denying(
                "This process is on the protected-critical list and cannot be ended.",
                new ActionRisk { IsCritical = true, IsSystem = true });
        }

        var risk = new ActionRisk { VisibleWindows = 2, ChildCount = 3 };
        risk.Notes.Add("Child processes will keep running (they become orphans).");
        risk.Notes.Add("Unsaved work in visible windows may be lost.");
        return FakeActionBroker.Allowing(risk, executeMessage: "The action completed.");
    }
}
