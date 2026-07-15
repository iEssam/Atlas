using System.Collections.ObjectModel;
using Atlas.IpcClient;
using Atlas.V0;
using CommunityToolkit.Mvvm.ComponentModel;
using Microsoft.UI.Dispatching;

namespace Atlas.App.ViewModels;

/// <summary>Live, adapter-aware GPU telemetry. Detailed adapter data uses the
/// gRPC stream because shared-memory v2 intentionally carries only aggregates.</summary>
public sealed partial class GpuViewModel : ObservableObject
{
    private readonly DispatcherQueue _dispatcher;
    private readonly string? _who;
    private CancellationTokenSource? _cts;

    public ObservableCollection<GpuAdapterItem> Adapters { get; } = new();
    public ObservableCollection<GpuProcessItem> Processes { get; } = new();
    [ObservableProperty] private GpuAdapterItem? _selectedAdapter;
    [ObservableProperty] private string _statusText = "Connecting to GPU telemetry";
    [ObservableProperty] private bool _isUnavailable;
    [ObservableProperty] private string _unavailableReason = string.Empty;

    public GpuViewModel(DispatcherQueue dispatcher, string? who = null)
    {
        _dispatcher = dispatcher;
        _who = who;
    }

    public void Start()
    {
        Stop();
        _cts = new CancellationTokenSource();
        _ = RunAsync(_cts.Token);
    }

    public void Stop() { _cts?.Cancel(); _cts?.Dispose(); _cts = null; }

    private async Task RunAsync(CancellationToken ct)
    {
        try
        {
            using var channel = AtlasChannel.Connect(_who);
            await foreach (var snapshot in channel.StreamSnapshotsAsync(cancellationToken: ct))
            {
                if (ct.IsCancellationRequested) break;
                _dispatcher.TryEnqueue(() => Apply(snapshot));
            }
        }
        catch (OperationCanceledException) { }
        catch (Exception ex)
        {
            _dispatcher.TryEnqueue(() =>
            {
                IsUnavailable = true;
                UnavailableReason = $"GPU telemetry could not be read: {ex.Message}";
                StatusText = "Unavailable";
            });
        }
    }

    private void Apply(SnapshotReply snapshot)
    {
        string? selectedKey = SelectedAdapter?.AdapterKey;
        var seen = new HashSet<string>();
        foreach (var adapter in snapshot.GpuAdapters)
        {
            seen.Add(adapter.AdapterKey);
            var item = Adapters.FirstOrDefault(a => a.AdapterKey == adapter.AdapterKey);
            if (item is null) { item = new GpuAdapterItem(adapter.AdapterKey); Adapters.Add(item); }
            item.Apply(adapter);
        }
        for (int i = Adapters.Count - 1; i >= 0; i--)
            if (!seen.Contains(Adapters[i].AdapterKey)) Adapters.RemoveAt(i);

        SelectedAdapter = Adapters.FirstOrDefault(a => a.AdapterKey == selectedKey)
            ?? Adapters.FirstOrDefault(a => a.ActiveDisplay)
            ?? Adapters.FirstOrDefault();

        Processes.Clear();
        foreach (var p in snapshot.Processes
            .Where(p => p.GpuPermille > 0 || p.GpuDedicatedBytes > 0 || p.GpuSharedBytes > 0)
            .OrderByDescending(p => p.GpuPermille).Take(50))
        {
            Processes.Add(new GpuProcessItem(p));
        }

        IsUnavailable = Adapters.Count == 0;
        UnavailableReason = IsUnavailable
            ? (string.IsNullOrWhiteSpace(snapshot.GpuUnavailableReason)
                ? "Windows did not expose GPU counters for this session."
                : snapshot.GpuUnavailableReason)
            : string.Empty;
        StatusText = IsUnavailable ? "No measured GPU data" : $"{Adapters.Count} adapter{(Adapters.Count == 1 ? "" : "s")} measured live";
    }
}

public sealed partial class GpuAdapterItem : ObservableObject
{
    public string AdapterKey { get; }
    public ObservableCollection<GpuEngineItem> Engines { get; } = new();
    [ObservableProperty] private string _name = "GPU";
    [ObservableProperty] private string _driverVersion = string.Empty;
    [ObservableProperty] private bool _activeDisplay;
    [ObservableProperty] private double _utilizationPercent;
    [ObservableProperty] private string _dedicatedText = "0 MB measured";
    [ObservableProperty] private string _sharedText = "0 MB measured";
    [ObservableProperty] private string _temperatureText = "Not exposed";
    [ObservableProperty] private string _powerText = "Not exposed";
    [ObservableProperty] private string _clockText = "Not exposed";
    [ObservableProperty] private string _fanText = "Not exposed";
    [ObservableProperty] private string _throttleText = "Not exposed";
    [ObservableProperty] private string _sensorStatus = string.Empty;
    public string UtilizationText => $"{UtilizationPercent:F1} %";
    public string DisplayRole => ActiveDisplay ? "Active display adapter" : "Available adapter";

    public GpuAdapterItem(string key) => AdapterKey = key;

    public void Apply(GpuAdapterTelemetry a)
    {
        Name = a.Name; DriverVersion = a.DriverVersion; ActiveDisplay = a.ActiveDisplay;
        UtilizationPercent = a.UtilizationPermille / 10.0;
        OnPropertyChanged(nameof(UtilizationText)); OnPropertyChanged(nameof(DisplayRole));
        DedicatedText = MemoryLine(a.DedicatedUsed, a.DedicatedBudget);
        SharedText = MemoryLine(a.SharedUsed, a.SharedBudget);
        TemperatureText = a.HasTemperatureC ? $"{a.TemperatureC:F1} °C" : "Not exposed";
        PowerText = a.HasPowerW ? $"{a.PowerW:F1} W" : "Not exposed";
        ClockText = a.HasCoreClockMhz ? $"{a.CoreClockMhz} MHz core" : "Not exposed";
        FanText = a.HasFanRpm ? $"{a.FanRpm} RPM" : "Not exposed";
        ThrottleText = a.HasThermalThrottling ? (a.ThermalThrottling ? "Throttling reported" : "No throttling reported") : "Not exposed";
        SensorStatus = string.IsNullOrWhiteSpace(a.SensorSource) ? a.SensorUnavailableReason : $"Sensor source: {a.SensorSource}";
        Engines.Clear();
        foreach (var e in a.Engines.OrderBy(e => e.EngineClass)) Engines.Add(new GpuEngineItem(e));
    }

    private static string MemoryLine(ulong used, ulong budget) => budget > 0
        ? $"{used / 1048576.0:F0} / {budget / 1048576.0:F0} MB"
        : $"{used / 1048576.0:F0} MB measured - budget unavailable";
}

public sealed class GpuEngineItem
{
    public string Name { get; }
    public double Percent { get; }
    public string PercentText => $"{Percent:F1} %";
    public GpuEngineItem(GpuEngineTelemetry e)
    {
        Name = e.EngineClass switch
        {
            GpuEngineClass.GpuEngine3D => "3D", GpuEngineClass.GpuEngineCompute => "Compute",
            GpuEngineClass.GpuEngineCopy => "Copy", GpuEngineClass.GpuEngineVideoEncode => "Video encode",
            GpuEngineClass.GpuEngineVideoDecode => "Video decode", _ => "Other",
        };
        Percent = e.UtilizationPermille / 10.0;
    }
}

public sealed class GpuProcessItem
{
    public string Name { get; }
    public uint Pid { get; }
    public string PidText => Pid.ToString();
    public string UsageText { get; }
    public string DedicatedText { get; }
    public string SharedText { get; }
    public GpuProcessItem(ProcessRow p)
    {
        Name = p.ImageName; Pid = p.Pid; UsageText = $"{p.GpuPermille / 10.0:F1} %";
        DedicatedText = $"{p.GpuDedicatedBytes / 1048576.0:F0} MB";
        SharedText = $"{p.GpuSharedBytes / 1048576.0:F0} MB";
    }
}
