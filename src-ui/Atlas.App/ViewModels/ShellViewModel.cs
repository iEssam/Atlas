using System;
using Atlas.App.Services;
using CommunityToolkit.Mvvm.ComponentModel;
using Microsoft.UI;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml.Media;

namespace Atlas.App.ViewModels;

/// <summary>
/// Drives the shell's top status bar and sidebar footer. Every value here is
/// REAL: the device/user come from the environment, and the capture/connection
/// state is the live source-selection status reported by
/// <see cref="LiveMetricsService"/> (ring vs. stream vs. none). No event-index
/// counts or storage-health verdicts are shown, because the app has no truthful
/// source for those — inventing them is not allowed.
/// </summary>
public sealed partial class ShellViewModel : ObservableObject
{
    private readonly LiveMetricsService _metrics;

    /// <summary>Machine name — the "device" in the status bar.</summary>
    public string DeviceName { get; } = Environment.MachineName;

    /// <summary>Interactive user — the "session" owner.</summary>
    public string UserName { get; } = Environment.UserName;

    /// <summary>"DEVICE / NAME" mono label for the top bar.</summary>
    public string DeviceLabel => $"DEVICE / {DeviceName.ToUpperInvariant()}";

    /// <summary>"SESSION / USER" mono label for the top bar.</summary>
    public string SessionLabel => $"SESSION / {UserName.ToUpperInvariant()}";

    // Status-dot brushes (mineral cyan when live, subdued amber when not).
    private static readonly Brush CyanBrush = new SolidColorBrush(ColorHelper.FromArgb(0xFF, 0x7F, 0xC6, 0xC0));
    private static readonly Brush AmberBrush = new SolidColorBrush(ColorHelper.FromArgb(0xFF, 0xC3, 0xA0, 0x6A));

    /// <summary>True when a live source (ring or stream) is connected.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(CaptureStateText))]
    [NotifyPropertyChangedFor(nameof(CaptureBrush))]
    private bool _isCaptureActive;

    /// <summary>Status-dot brush: cyan when capturing, amber when offline.</summary>
    public Brush CaptureBrush => IsCaptureActive ? CyanBrush : AmberBrush;

    /// <summary>Raw status text from the metrics service (diagnostic detail).</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(SourceText))]
    private string _connectionStatus = "Connecting…";

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(SourceText))]
    private MetricsSource _source = MetricsSource.None;

    /// <summary>Short capture-state word for the status bar.</summary>
    public string CaptureStateText => IsCaptureActive
        ? "CAPTURE ACTIVE"
        : (ConnectionStatus.StartsWith("Connecting", StringComparison.OrdinalIgnoreCase)
            ? "CONNECTING…"
            : "CAPTURE OFFLINE");

    /// <summary>Which real source is feeding live data.</summary>
    public string SourceText => Source switch
    {
        MetricsSource.Ring => "SOURCE / RING",
        MetricsSource.Stream => "SOURCE / STREAM",
        _ => "SOURCE / —",
    };

    public ShellViewModel(DispatcherQueue dispatcher)
    {
        _metrics = new LiveMetricsService(dispatcher);
        _metrics.StatusChanged += OnStatusChanged;
        _metrics.Start();
    }

    private void OnStatusChanged(MetricsSource source, string status)
    {
        Source = source;
        ConnectionStatus = status;
        IsCaptureActive = source is MetricsSource.Ring or MetricsSource.Stream;
    }

    /// <summary>Stops the shell status poller (call on window close).</summary>
    public void Stop() => _metrics.Stop();
}
