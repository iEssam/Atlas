using System;
using System.Collections.ObjectModel;
using System.Threading;
using System.Threading.Tasks;
using Atlas.IpcClient;
using Atlas.V0;
using CommunityToolkit.Mvvm.ComponentModel;
using Microsoft.UI.Dispatching;

namespace Atlas.App.ViewModels;

/// <summary>
/// Drives the Sensors page (R2, PRD §9.6.6 / §9.6.7): three hardware-dependent
/// cards — Battery, Thermal, and Startup history (boots). Each is fetched
/// independently and each honours the honesty principle: an absent battery reads
/// as "No battery — desktop system", a machine that exposes no thermal sensors says
/// so plainly, and a missing boot log is stated without implying a fault. None of
/// these is an error state.
///
/// <para>
/// Every card distinguishes three outcomes: the RPC is unsupported (service too
/// old → "needs a newer Atlas"), the reply reports <c>available = false</c> (the
/// calm hardware-absent note, using the service's own reason), or data is present.
/// </para>
/// </summary>
public sealed partial class SensorsViewModel : ObservableObject
{
    private const uint BootLimit = 10;

    private readonly DispatcherQueue _dispatcher;
    private readonly string? _who;
    private CancellationTokenSource? _cts;

    [ObservableProperty] private bool _isLoading;

    // ---- Battery card ------------------------------------------------------
    [ObservableProperty] private bool _batteryUnsupported;
    [ObservableProperty] private bool _batteryAbsent;
    [ObservableProperty] private bool _batteryPresent;
    [ObservableProperty] private string _batteryNote = string.Empty;
    [ObservableProperty] private string _batteryPercentText = string.Empty;
    [ObservableProperty] private int _batteryPercentValue;
    [ObservableProperty] private string _batteryStateText = string.Empty;
    [ObservableProperty] private string _batteryHealthText = string.Empty;
    [ObservableProperty] private string _batteryCycleText = string.Empty;

    // ---- Thermal card ------------------------------------------------------
    [ObservableProperty] private bool _thermalUnsupported;
    [ObservableProperty] private bool _thermalAbsent;
    [ObservableProperty] private bool _thermalPresent;
    [ObservableProperty] private string _thermalNote = string.Empty;

    public ObservableCollection<ThermalSensorItem> Sensors { get; } = new();

    // ---- GPU vendor sensor coverage --------------------------------------
    [ObservableProperty] private bool _gpuAbsent;
    [ObservableProperty] private bool _gpuPresent;
    [ObservableProperty] private string _gpuNote = string.Empty;
    public ObservableCollection<GpuSensorItem> GpuSensors { get; } = new();

    // ---- Startup history (boots) card -------------------------------------
    [ObservableProperty] private bool _bootsUnsupported;
    [ObservableProperty] private bool _bootsAbsent;
    [ObservableProperty] private bool _bootsPresent;
    [ObservableProperty] private string _bootsNote = string.Empty;

    public ObservableCollection<BootItem> Boots { get; } = new();

    public SensorsViewModel(DispatcherQueue dispatcher, string? who = null)
    {
        _dispatcher = dispatcher;
        _who = who;
    }

    /// <summary>Loads all three cards. Each degrades independently.</summary>
    public async Task RefreshAsync()
    {
        _cts?.Cancel();
        var cts = new CancellationTokenSource();
        _cts = cts;
        var ct = cts.Token;

        IsLoading = true;
        try
        {
            using var channel = AtlasChannel.Connect(_who);
            await Task.WhenAll(
                LoadBatteryAsync(channel, ct),
                LoadThermalAsync(channel, ct),
                LoadGpuAsync(channel, ct),
                LoadBootsAsync(channel, ct)).ConfigureAwait(false);
        }
        catch (OperationCanceledException)
        {
            // Superseded by a newer refresh / navigation away.
        }
        finally
        {
            Post(() => IsLoading = false);
        }
    }

    private async Task LoadBatteryAsync(AtlasChannel channel, CancellationToken ct)
    {
        RpcOutcome<GetBatteryStatusReply> outcome;
        try
        {
            outcome = await channel.GetBatteryStatusAsync(ct).ConfigureAwait(false);
        }
        catch (Exception ex) when (ex is not OperationCanceledException)
        {
            Post(() => SetBatteryUnsupported($"Could not reach the service: {ex.Message}"));
            return;
        }
        if (ct.IsCancellationRequested)
        {
            return;
        }

        Post(() =>
        {
            if (!outcome.Supported)
            {
                SetBatteryUnsupported(
                    "Battery status needs a newer Atlas — the connected service is too old.");
                return;
            }

            var reply = outcome.Value;
            if (!reply.Available || reply.Status is null || !reply.Status.Present)
            {
                BatteryUnsupported = false;
                BatteryPresent = false;
                BatteryAbsent = true;
                BatteryNote = MonitorFormatter.UnavailableReason(
                    reply.UnavailableReason, "No battery — desktop system.");
                return;
            }

            var s = reply.Status;
            BatteryUnsupported = false;
            BatteryAbsent = false;
            BatteryPresent = true;
            BatteryPercentText = MonitorFormatter.BatteryPercentText(s.Percent);
            BatteryPercentValue = (int)Math.Clamp(s.Percent, 0u, 100u);
            BatteryStateText = MonitorFormatter.BatteryStateSummary(
                s.Charging, s.OnAc, s.RateMw, s.EstRuntimeS);
            BatteryHealthText = MonitorFormatter.BatteryHealthText(s.HealthPercent);
            BatteryCycleText = MonitorFormatter.CycleCountText(s.CycleCount);
        });
    }

    private void SetBatteryUnsupported(string note)
    {
        BatteryPresent = false;
        BatteryAbsent = false;
        BatteryUnsupported = true;
        BatteryNote = note;
    }

    private async Task LoadThermalAsync(AtlasChannel channel, CancellationToken ct)
    {
        RpcOutcome<GetThermalReply> outcome;
        try
        {
            outcome = await channel.GetThermalAsync(ct).ConfigureAwait(false);
        }
        catch (Exception ex) when (ex is not OperationCanceledException)
        {
            Post(() => SetThermalUnsupported($"Could not reach the service: {ex.Message}"));
            return;
        }
        if (ct.IsCancellationRequested)
        {
            return;
        }

        Post(() =>
        {
            Sensors.Clear();
            if (!outcome.Supported)
            {
                SetThermalUnsupported(
                    "Thermal readings need a newer Atlas — the connected service is too old.");
                return;
            }

            var reply = outcome.Value;
            if (!reply.Available || reply.Sensors.Count == 0)
            {
                ThermalUnsupported = false;
                ThermalPresent = false;
                ThermalAbsent = true;
                ThermalNote = MonitorFormatter.UnavailableReason(
                    reply.UnavailableReason, "No thermal sensors exposed by this hardware.");
                return;
            }

            ThermalUnsupported = false;
            ThermalAbsent = false;
            ThermalPresent = true;
            foreach (var sensor in reply.Sensors)
            {
                Sensors.Add(new ThermalSensorItem(
                    string.IsNullOrWhiteSpace(sensor.Name) ? "Sensor" : sensor.Name,
                    MonitorFormatter.TemperatureText(sensor.Celsius),
                    MonitorFormatter.ThermalSourceText(sensor.Source)));
            }
        });
    }

    private void SetThermalUnsupported(string note)
    {
        ThermalPresent = false;
        ThermalAbsent = false;
        ThermalUnsupported = true;
        ThermalNote = note;
    }

    private async Task LoadGpuAsync(AtlasChannel channel, CancellationToken ct)
    {
        try
        {
            var reply = await channel.GetSnapshotAsync(0, ct).ConfigureAwait(false);
            if (ct.IsCancellationRequested) return;
            Post(() =>
            {
                GpuSensors.Clear();
                foreach (var a in reply.GpuAdapters)
                {
                    var readings = new List<string>();
                    AddGpuReading(readings, a, GpuSensorKind.GpuSensorCoreTemperature, a.HasTemperatureC ? $"core {a.TemperatureC:F1} °C" : null);
                    AddGpuReading(readings, a, GpuSensorKind.GpuSensorPowerWatts, a.HasPowerW ? $"draw {a.PowerW:F1} W" : null);
                    AddGpuReading(readings, a, GpuSensorKind.GpuSensorPowerPercent, a.HasPowerPercent ? $"power load {a.PowerPercent:F1} %" : null);
                    AddGpuReading(readings, a, GpuSensorKind.GpuSensorCoreClock, a.HasCoreClockMhz ? $"core {a.CoreClockMhz} MHz" : null);
                    AddGpuReading(readings, a, GpuSensorKind.GpuSensorMemoryClock, a.HasMemoryClockMhz ? $"memory {a.MemoryClockMhz} MHz" : null);
                    AddGpuReading(readings, a, GpuSensorKind.GpuSensorFanRpm, a.HasFanRpm ? $"fan {a.FanRpm} RPM" : null);
                    AddGpuReading(readings, a, GpuSensorKind.GpuSensorFanPercent, a.HasFanPercent ? $"fan target {a.FanPercent:F1} %" : null);
                    readings.AddRange(a.Temperatures
                        .Where(t => t.Kind != GpuTemperatureKind.GpuTemperatureCore)
                        .Select(t => $"{t.Label} {t.Celsius:F1} °C · {GpuAdapterItem.SourceName(t.Source)}"));
                    var unavailable = a.SensorAvailability
                        .Where(v => !v.Available)
                        .Select(v => $"{GpuAdapterItem.SourceName(v.Source)} {v.Kind.ToString().Replace("GpuSensor", string.Empty)}: {GpuAdapterItem.ReasonCode(v.Reason)}")
                        .Distinct()
                        .ToList();
                    var source = unavailable.Count == 0
                        ? (string.IsNullOrWhiteSpace(a.SensorSource) ? "No provider status was reported." : a.SensorSource)
                        : string.Join(" · ", unavailable);
                    GpuSensors.Add(new GpuSensorItem(
                        string.IsNullOrWhiteSpace(a.Name) ? "GPU" : a.Name,
                        string.Join(" · ", readings), source));
                }
                GpuPresent = GpuSensors.Count > 0;
                GpuAbsent = !GpuPresent;
                GpuNote = GpuAbsent
                    ? MonitorFormatter.UnavailableReason(reply.GpuUnavailableReason, "Windows did not expose GPU adapters for this session.")
                    : string.Empty;
            });
        }
        catch (Exception ex) when (ex is not OperationCanceledException)
        {
            Post(() =>
            {
                GpuSensors.Clear();
                GpuPresent = false;
                GpuAbsent = true;
                GpuNote = $"GPU sensor coverage could not be read: {ex.Message}";
            });
        }
    }

    private static void AddGpuReading(List<string> readings, GpuAdapterTelemetry adapter, GpuSensorKind kind, string? value)
    {
        if (value is null) return;
        var source = adapter.SensorAvailability
            .Where(v => v.Kind == kind && v.Available)
            .OrderByDescending(v => v.Source == GpuTelemetrySource.GpuSourceNvidiaNvml)
            .Select(v => GpuAdapterItem.SourceName(v.Source))
            .FirstOrDefault();
        readings.Add(string.IsNullOrWhiteSpace(source) ? value : $"{value} · {source}");
    }

    private async Task LoadBootsAsync(AtlasChannel channel, CancellationToken ct)
    {
        RpcOutcome<ListBootsReply> outcome;
        try
        {
            outcome = await channel.ListBootsAsync(BootLimit, ct).ConfigureAwait(false);
        }
        catch (Exception ex) when (ex is not OperationCanceledException)
        {
            Post(() => SetBootsUnsupported($"Could not reach the service: {ex.Message}"));
            return;
        }
        if (ct.IsCancellationRequested)
        {
            return;
        }

        Post(() =>
        {
            Boots.Clear();
            if (!outcome.Supported)
            {
                SetBootsUnsupported(
                    "Startup history needs a newer Atlas — the connected service is too old.");
                return;
            }

            var reply = outcome.Value;
            if (!reply.Available || reply.Boots.Count == 0)
            {
                BootsUnsupported = false;
                BootsPresent = false;
                BootsAbsent = true;
                BootsNote = MonitorFormatter.UnavailableReason(
                    reply.UnavailableReason, "No boot history is available on this system.");
                return;
            }

            BootsUnsupported = false;
            BootsAbsent = false;
            BootsPresent = true;
            foreach (var b in reply.Boots)
            {
                Boots.Add(new BootItem(
                    MonitorFormatter.BootTimeText(b.BootMs),
                    MonitorFormatter.BootDurationText(b.BootDurationMs),
                    MonitorFormatter.BootDegradedText(b.Degraded),
                    MonitorFormatter.BootDegradedToken(b.Degraded)));
            }
        });
    }

    private void SetBootsUnsupported(string note)
    {
        BootsPresent = false;
        BootsAbsent = false;
        BootsUnsupported = true;
        BootsNote = note;
    }

    public void Stop() => _cts?.Cancel();

    private void Post(Action action) => _dispatcher.TryEnqueue(() => action());
}

/// <summary>One thermal sensor reading, pre-formatted for the list.</summary>
public sealed class ThermalSensorItem
{
    public string Name { get; }
    public string TemperatureText { get; }
    public string SourceText { get; }

    public ThermalSensorItem(string name, string temperatureText, string sourceText)
    {
        Name = name;
        TemperatureText = temperatureText;
        SourceText = sourceText;
    }
}

public sealed class GpuSensorItem
{
    public string Name { get; }
    public string ReadingsText { get; }
    public string CoverageText { get; }

    public GpuSensorItem(string name, string readingsText, string coverageText)
    {
        Name = name;
        ReadingsText = readingsText;
        CoverageText = coverageText;
    }
}

/// <summary>One boot record, pre-formatted for the startup-history list.</summary>
public sealed class BootItem
{
    public string TimeText { get; }
    public string DurationText { get; }
    public string DegradedText { get; }

    /// <summary>Calm color token for the degraded flag ("ok"/"attention").</summary>
    public string DegradedToken { get; }

    public BootItem(string timeText, string durationText, string degradedText, string degradedToken)
    {
        TimeText = timeText;
        DurationText = durationText;
        DegradedText = degradedText;
        DegradedToken = degradedToken;
    }
}
