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
            Processes.Add(new GpuProcessItem(p));

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
    public ObservableCollection<GpuTemperatureItem> AdditionalTemperatures { get; } = new();
    public ObservableCollection<GpuAvailabilityItem> Availability { get; } = new();
    [ObservableProperty] private string _name = "GPU";
    [ObservableProperty] private string _driverVersion = string.Empty;
    [ObservableProperty] private string _driverDate = string.Empty;
    [ObservableProperty] private string _pciLocation = "PCI location unavailable";
    [ObservableProperty] private string _adapterIdentity = string.Empty;
    [ObservableProperty] private bool _activeDisplay;
    [ObservableProperty] private double _utilizationPercent;
    [ObservableProperty] private string _dedicatedText = "0 MB measured";
    [ObservableProperty] private string _sharedText = "0 MB measured";
    [ObservableProperty] private string _temperatureText = "Unavailable";
    [ObservableProperty] private string _powerWattsText = "Unavailable";
    [ObservableProperty] private string _powerPercentText = "Unavailable";
    [ObservableProperty] private string _coreClockText = "Unavailable";
    [ObservableProperty] private string _memoryClockText = "Unavailable";
    [ObservableProperty] private string _fanRpmText = "Unavailable";
    [ObservableProperty] private string _fanPercentText = "Unavailable";
    [ObservableProperty] private string _temperatureLimitsText = "Limits unavailable";
    [ObservableProperty] private string _throttleText = "Unavailable";
    [ObservableProperty] private string _sensorStatus = string.Empty;
    public string UtilizationText => $"{UtilizationPercent:F1} %";
    public string DisplayRole => ActiveDisplay ? "Active display adapter" : "Available adapter";

    public GpuAdapterItem(string key) => AdapterKey = key;

    public void Apply(GpuAdapterTelemetry a)
    {
        Name = a.Name;
        DriverVersion = string.IsNullOrWhiteSpace(a.DriverVersion) ? "Driver version unavailable" : $"Driver {a.DriverVersion}";
        DriverDate = string.IsNullOrWhiteSpace(a.DriverDate) ? "Driver date unavailable" : $"Driver date {a.DriverDate}";
        ActiveDisplay = a.ActiveDisplay;
        PciLocation = a.PciIdentityAvailable
            ? $"PCI {a.PciDomain:X4}:{a.PciBus:X2}:{a.PciDevice:X2}.{a.PciFunction}"
            : "PCI location unavailable";
        AdapterIdentity = $"VEN_{a.VendorId:X4} · DEV_{a.DeviceId:X4} · physical {a.PhysicalAdapterIndex}";
        UtilizationPercent = a.UtilizationPermille / 10.0;
        OnPropertyChanged(nameof(UtilizationText)); OnPropertyChanged(nameof(DisplayRole));
        DedicatedText = MemoryLine(a.DedicatedUsed, a.DedicatedBudget);
        SharedText = MemoryLine(a.SharedUsed, a.SharedBudget);
        TemperatureText = Reading(a.HasTemperatureC, a.HasTemperatureC ? $"{a.TemperatureC:F1} °C" : null, a, GpuSensorKind.GpuSensorCoreTemperature);
        PowerWattsText = Reading(a.HasPowerW, a.HasPowerW ? $"{a.PowerW:F1} W" : null, a, GpuSensorKind.GpuSensorPowerWatts);
        PowerPercentText = Reading(a.HasPowerPercent, a.HasPowerPercent ? $"{a.PowerPercent:F1} %" : null, a, GpuSensorKind.GpuSensorPowerPercent);
        CoreClockText = Reading(a.HasCoreClockMhz, a.HasCoreClockMhz ? $"{a.CoreClockMhz} MHz" : null, a, GpuSensorKind.GpuSensorCoreClock);
        MemoryClockText = Reading(a.HasMemoryClockMhz, a.HasMemoryClockMhz ? $"{a.MemoryClockMhz} MHz" : null, a, GpuSensorKind.GpuSensorMemoryClock);
        FanRpmText = Reading(a.HasFanRpm, a.HasFanRpm ? $"{a.FanRpm} RPM" : null, a, GpuSensorKind.GpuSensorFanRpm);
        FanPercentText = Reading(a.HasFanPercent, a.HasFanPercent ? $"{a.FanPercent:F1} %" : null, a, GpuSensorKind.GpuSensorFanPercent);
        TemperatureLimitsText = TemperatureLimits(a);
        ThrottleText = ThrottleState(a);
        SensorStatus = ProviderStatus(a);
        Engines.Clear();
        foreach (var e in a.Engines.OrderBy(e => e.EngineClass)) Engines.Add(new GpuEngineItem(e));
        AdditionalTemperatures.Clear();
        foreach (var temperature in a.Temperatures.Where(t => t.Kind != GpuTemperatureKind.GpuTemperatureCore))
            AdditionalTemperatures.Add(new GpuTemperatureItem(temperature));
        Availability.Clear();
        foreach (var availability in a.SensorAvailability.Where(v => !v.Available).OrderBy(v => v.Source).ThenBy(v => v.Kind))
            Availability.Add(new GpuAvailabilityItem(availability));
    }

    private static string Reading(bool present, string? value, GpuAdapterTelemetry adapter, GpuSensorKind kind)
    {
        var available = adapter.SensorAvailability.Where(v => v.Kind == kind && v.Available)
            .OrderByDescending(v => v.Source == GpuTelemetrySource.GpuSourceNvidiaNvml).FirstOrDefault();
        if (present && value is not null)
            return available is null ? value : $"{value} · {SourceName(available.Source)}";
        var unavailable = adapter.SensorAvailability.Where(v => v.Kind == kind && !v.Available)
            .OrderByDescending(v => v.Source == GpuTelemetrySource.GpuSourceNvidiaNvml).FirstOrDefault();
        return unavailable is null ? "Unavailable" : $"Unavailable · {ReasonCode(unavailable.Reason)}";
    }

    private static string TemperatureLimits(GpuAdapterTelemetry adapter)
    {
        var parts = new List<string>();
        if (adapter.HasTemperatureWarningC) parts.Add($"warning {adapter.TemperatureWarningC:F1} °C");
        if (adapter.HasTemperatureMaxC) parts.Add($"maximum {adapter.TemperatureMaxC:F1} °C");
        return parts.Count == 0 ? "Limits unavailable" : $"{string.Join(" · ", parts)} · Windows WDDM";
    }

    private static string ThrottleState(GpuAdapterTelemetry adapter)
    {
        if (!adapter.HasThermalThrottling)
            return Reading(false, null, adapter, GpuSensorKind.GpuSensorThrottleReasons);
        if (adapter.ThrottleReasons.Count == 0)
            return "No explicit thermal throttle · NVIDIA NVML";
        return $"{string.Join(", ", adapter.ThrottleReasons.Select(ThrottleName))} · NVIDIA NVML";
    }

    private static string ProviderStatus(GpuAdapterTelemetry adapter)
    {
        bool nvmlActive = adapter.SensorAvailability.Any(v => v.Source == GpuTelemetrySource.GpuSourceNvidiaNvml && v.Available);
        var nvmlFailure = adapter.SensorAvailability.FirstOrDefault(v =>
            v.Source == GpuTelemetrySource.GpuSourceNvidiaNvml && !v.Available &&
            v.Reason != GpuAvailabilityReason.GpuAvailabilityUnsupportedMetric);
        if (nvmlActive) return "NVIDIA NVML is active. Unsupported fields remain on their current Windows WDDM reading.";
        if (nvmlFailure is not null)
            return $"Windows WDDM fallback · {ReasonCode(nvmlFailure.Reason)} · {nvmlFailure.Detail}";
        return string.IsNullOrWhiteSpace(adapter.SensorSource) ? adapter.SensorUnavailableReason : adapter.SensorSource;
    }

    internal static string SourceName(GpuTelemetrySource source) => MonitorFormatter.GpuSourceText(source);

    internal static string ReasonCode(GpuAvailabilityReason reason) => MonitorFormatter.GpuAvailabilityCode(reason);

    private static string ThrottleName(GpuThrottleReason reason) => reason switch
    {
        GpuThrottleReason.GpuThrottleSoftwareThermal => "software thermal limit",
        GpuThrottleReason.GpuThrottleHardwareThermal => "hardware thermal limit",
        GpuThrottleReason.GpuThrottleSoftwarePowerCap => "software power cap",
        GpuThrottleReason.GpuThrottleHardwareSlowdown => "hardware slowdown",
        GpuThrottleReason.GpuThrottleHardwarePowerBrake => "hardware power brake",
        GpuThrottleReason.GpuThrottleIdle => "GPU idle",
        GpuThrottleReason.GpuThrottleApplicationClocks => "application clock setting",
        GpuThrottleReason.GpuThrottleSyncBoost => "sync boost",
        GpuThrottleReason.GpuThrottleDisplayClockSetting => "display clock setting",
        _ => "other hardware reason",
    };

    private static string MemoryLine(ulong used, ulong budget) => budget > 0
        ? $"{used / 1048576.0:F0} / {budget / 1048576.0:F0} MB"
        : $"{used / 1048576.0:F0} MB measured - budget unavailable";
}

public sealed class GpuTemperatureItem
{
    public string Name { get; }
    public string ValueText { get; }
    public GpuTemperatureItem(GpuTemperatureTelemetry temperature)
    {
        Name = string.IsNullOrWhiteSpace(temperature.Label) ? temperature.Kind.ToString() : temperature.Label;
        ValueText = $"{temperature.Celsius:F1} °C · {GpuAdapterItem.SourceName(temperature.Source)}";
    }
}

public sealed class GpuAvailabilityItem
{
    public string Metric { get; }
    public string Detail { get; }
    public GpuAvailabilityItem(GpuSensorAvailability availability)
    {
        Metric = availability.Kind.ToString().Replace("GpuSensor", string.Empty);
        Detail = $"{GpuAdapterItem.SourceName(availability.Source)} · {GpuAdapterItem.ReasonCode(availability.Reason)}" +
            (string.IsNullOrWhiteSpace(availability.Detail) ? string.Empty : $" · {availability.Detail}");
    }
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
