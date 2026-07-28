using Atlas.App.Services;
using CommunityToolkit.Mvvm.ComponentModel;
using Microsoft.UI.Dispatching;

namespace Atlas.App.ViewModels;

/// <summary>
/// Exposes only measured shell state: the current device and the real live
/// metrics source. Status meaning is carried by text and glyph, never color
/// alone.
/// </summary>
public sealed partial class ShellViewModel : ObservableObject
{
    private readonly LiveMetricsService _metrics;

    public string DeviceName { get; } = Environment.MachineName;
    public string UserName { get; } = Environment.UserName;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(CaptureStateText))]
    [NotifyPropertyChangedFor(nameof(CaptureGlyph))]
    private bool _isCaptureActive;

    public string CaptureGlyph => IsCaptureActive ? "\uE930" : "\uE7BA";

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(SourceText))]
    private string _connectionStatus = "Connecting...";

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(SourceText))]
    private MetricsSource _source = MetricsSource.None;

    public string CaptureStateText => IsCaptureActive
        ? "Capture active"
        : (ConnectionStatus.StartsWith("Connecting", StringComparison.OrdinalIgnoreCase)
            ? "Connecting..."
            : "Capture offline");

    public string SourceText => Source switch
    {
        MetricsSource.Ring => "Ring buffer",
        MetricsSource.Stream => "Service stream",
        _ => "No live source",
    };

    public ShellViewModel(DispatcherQueue dispatcher, string? who = null)
    {
        _metrics = new LiveMetricsService(dispatcher, who);
        _metrics.StatusChanged += OnStatusChanged;
        _metrics.Start();
    }

    private void OnStatusChanged(MetricsSource source, string status)
    {
        Source = source;
        ConnectionStatus = status;
        IsCaptureActive = source is MetricsSource.Ring or MetricsSource.Stream;
    }

    public void Stop() => _metrics.Stop();
}
