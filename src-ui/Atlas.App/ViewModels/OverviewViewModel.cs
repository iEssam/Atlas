using System.Collections.ObjectModel;
using Atlas.App.Services;
using Atlas.IpcClient;
using Atlas.V0;
using CommunityToolkit.Mvvm.ComponentModel;
using Microsoft.UI.Dispatching;

namespace Atlas.App.ViewModels;

/// <summary>
/// Drives the approachable Overview surface: current measured values, a real
/// fifteen-minute trace, recent evidence, and the top CPU consumers. It never
/// invents a health score or a diagnosis.
/// </summary>
public sealed partial class OverviewViewModel : ObservableObject
{
    private const int TopConsumers = 5;
    private readonly LiveMetricsService _metrics;
    private readonly DispatcherQueue _dispatcher;
    private readonly string? _who;
    private CancellationTokenSource? _traceCts;
    private bool _traceRequested;

    public ObservableCollection<ConsumerRowViewModel> TopConsumers5 { get; } = new();
    public ObservableCollection<OverviewTracePoint> CpuTrace { get; } = new();
    public ObservableCollection<OverviewTracePoint> MemoryTrace { get; } = new();
    public ObservableCollection<OverviewTracePoint> GpuTrace { get; } = new();
    public ObservableCollection<OverviewEvidenceMarker> Evidence { get; } = new();

    [ObservableProperty] private string _connectionStatus = "Disconnected";
    [ObservableProperty] private bool _isTraceLoading;
    [ObservableProperty] private bool _isTraceUnavailable;
    [ObservableProperty] private string _traceStatus = "Waiting for the first measured sample.";

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(CpuText))]
    private double _cpuPercent;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(GpuText))]
    private double _gpuPercent;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(MemoryText))]
    [NotifyPropertyChangedFor(nameof(MemoryPercent))]
    private double _memUsedGb;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(MemoryText))]
    [NotifyPropertyChangedFor(nameof(MemoryPercent))]
    private double _memTotalGb;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(CommitText))]
    private double _commitUsedGb;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(CommitText))]
    private double _commitLimitGb;

    [ObservableProperty] private uint _processCount;
    [ObservableProperty] private uint _threadCount;
    [ObservableProperty] private uint _handleCount;

    public string CpuText => $"{CpuPercent:F1} %";
    public string GpuText => $"{GpuPercent:F1} %";
    public string MemoryText => $"{MemUsedGb:F1} / {MemTotalGb:F1} GB";
    public double MemoryPercent => MemTotalGb > 0 ? MemUsedGb / MemTotalGb * 100.0 : 0;
    public string CommitText => $"{CommitUsedGb:F1} / {CommitLimitGb:F1} GB";
    public string CountText => $"{ProcessCount:N0} processes, {ThreadCount:N0} threads, {HandleCount:N0} handles";
    public long TraceFromMs { get; private set; }
    public long TraceToMs { get; private set; }

    public event Action? TraceRefreshed;

    public OverviewViewModel(DispatcherQueue dispatcher, string? who = null)
    {
        _dispatcher = dispatcher;
        _who = who;
        _metrics = new LiveMetricsService(dispatcher, who);
        _metrics.StatusChanged += (_, status) => ConnectionStatus = status;
        _metrics.SnapshotReceived += Apply;
    }

    public void Start() => _metrics.Start();

    public void Stop()
    {
        _metrics.Stop();
        _traceCts?.Cancel();
    }

    public async Task RefreshTraceAsync()
    {
        if (MemTotalGb <= 0)
        {
            return;
        }

        _traceCts?.Cancel();
        var cts = new CancellationTokenSource();
        _traceCts = cts;
        var ct = cts.Token;
        _traceRequested = true;
        IsTraceLoading = true;
        IsTraceUnavailable = false;
        TraceStatus = "Loading the last 15 minutes of measured evidence.";

        var now = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
        var from = now - (long)TimeSpan.FromMinutes(15).TotalMilliseconds;
        TraceFromMs = from;
        TraceToMs = now;

        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var cpuTask = channel.QueryRangeAsync(MetricKind.SysCpuPermille, 0, from, now, 180, ct);
            var memoryTask = channel.QueryRangeAsync(MetricKind.SysMemUsed, 0, from, now, 180, ct);
            var gpuTask = channel.QueryRangeAsync(MetricKind.SysGpuPermille, 0, from, now, 180, ct);
            var incidentTask = channel.ListIncidentsAsync(from, now, 12, ct);
            var privacyTask = channel.ListPrivacyEventsAsync(from, now, 12, ct);
            var changesTask = channel.ListSystemChangesAsync(from, now, null, 12, ct);

            await Task.WhenAll(cpuTask, memoryTask, gpuTask, incidentTask, privacyTask, changesTask)
                .ConfigureAwait(false);
            if (ct.IsCancellationRequested)
            {
                return;
            }

            var cpu = await cpuTask;
            var memory = await memoryTask;
            var gpu = await gpuTask;
            var incidents = await incidentTask;
            var privacy = await privacyTask;
            var changes = await changesTask;

            if (!cpu.Supported || !memory.Supported || !gpu.Supported)
            {
                Post(() =>
                {
                    IsTraceUnavailable = true;
                    IsTraceLoading = false;
                    TraceStatus = "Historical trace unavailable from the connected service.";
                    ClearTrace();
                    TraceRefreshed?.Invoke();
                });
                return;
            }

            var totalBytes = MemTotalGb * 1024.0 * 1024.0 * 1024.0;
            var cpuPoints = ToPercentPoints(cpu.Value.Buckets, 10.0);
            var memoryPoints = ToPercentPoints(memory.Value.Buckets, totalBytes / 100.0);
            var gpuPoints = ToPercentPoints(gpu.Value.Buckets, 10.0);
            var evidence = new List<OverviewEvidenceMarker>();

            if (incidents.Supported)
            {
                evidence.AddRange(incidents.Value.Incidents.Select(item => new OverviewEvidenceMarker(
                    item.StartMs,
                    "Incident",
                    item.Summary,
                    FormatTime(item.StartMs))));
            }
            if (privacy.Supported)
            {
                evidence.AddRange(privacy.Value.Events.Select(item => new OverviewEvidenceMarker(
                    item.TsMs,
                    "Privacy",
                    $"{item.DisplayName}: {item.Capability} {(item.Started ? "started" : "stopped")}",
                    FormatTime(item.TsMs))));
            }
            if (changes.Supported)
            {
                evidence.AddRange(changes.Value.Changes.Select(item => new OverviewEvidenceMarker(
                    item.TsMs,
                    "Change",
                    $"{item.Subject}: {item.Detail}",
                    FormatTime(item.TsMs))));
            }
            evidence.Sort((left, right) => right.TimestampMs.CompareTo(left.TimestampMs));

            Post(() =>
            {
                Replace(CpuTrace, cpuPoints);
                Replace(MemoryTrace, memoryPoints);
                Replace(GpuTrace, gpuPoints);
                Replace(Evidence, evidence.Take(8));
                IsTraceLoading = false;
                TraceStatus = cpuPoints.Count == 0 && memoryPoints.Count == 0 && gpuPoints.Count == 0
                    ? "No historical samples have been recorded in this window yet."
                    : $"15 minute trace with {Evidence.Count} recent evidence marker{(Evidence.Count == 1 ? "" : "s")}.";
                TraceRefreshed?.Invoke();
            });
        }
        catch (OperationCanceledException)
        {
        }
        catch (Exception ex)
        {
            Post(() =>
            {
                IsTraceUnavailable = true;
                IsTraceLoading = false;
                TraceStatus = $"Could not load historical evidence: {ex.Message}";
                ClearTrace();
                TraceRefreshed?.Invoke();
            });
        }
    }

    private void Apply(MetricsSnapshot snap)
    {
        const double giga = 1024.0 * 1024.0 * 1024.0;
        CpuPercent = snap.CpuPercent;
        GpuPercent = snap.GpuPercent;
        MemUsedGb = snap.MemUsed / giga;
        MemTotalGb = snap.MemTotal / giga;
        CommitUsedGb = snap.CommitUsed / giga;
        CommitLimitGb = snap.CommitLimit / giga;
        ProcessCount = snap.ProcessCount;
        ThreadCount = snap.ThreadCount;
        HandleCount = snap.HandleCount;
        OnPropertyChanged(nameof(CountText));

        int n = Math.Min(TopConsumers, snap.Rows.Count);
        for (int i = 0; i < n; i++)
        {
            var row = snap.Rows[i];
            if (i < TopConsumers5.Count)
            {
                TopConsumers5[i].Update(row);
            }
            else
            {
                var vm = new ConsumerRowViewModel();
                vm.Update(row);
                TopConsumers5.Add(vm);
            }
        }
        for (int i = TopConsumers5.Count - 1; i >= n; i--)
        {
            TopConsumers5.RemoveAt(i);
        }

        if (!_traceRequested && MemTotalGb > 0)
        {
            _ = RefreshTraceAsync();
        }
    }

    private static List<OverviewTracePoint> ToPercentPoints(
        IEnumerable<RangeBucket> buckets,
        double divisor)
    {
        if (divisor <= 0)
        {
            return [];
        }

        return buckets
            .Where(bucket => bucket.Samples > 0)
            .Select(bucket => new OverviewTracePoint(
                bucket.StartMs,
                Math.Clamp(bucket.Avg / divisor, 0, 100)))
            .ToList();
    }

    private void ClearTrace()
    {
        CpuTrace.Clear();
        MemoryTrace.Clear();
        GpuTrace.Clear();
        Evidence.Clear();
    }

    private static void Replace<T>(ObservableCollection<T> target, IEnumerable<T> items)
    {
        target.Clear();
        foreach (var item in items)
        {
            target.Add(item);
        }
    }

    private static string FormatTime(long timestampMs) =>
        DateTimeOffset.FromUnixTimeMilliseconds(timestampMs).LocalDateTime.ToString("t");

    private void Post(Action action) => _dispatcher.TryEnqueue(() => action());
}

public readonly record struct OverviewTracePoint(long TimestampMs, double Percent);

public sealed record OverviewEvidenceMarker(
    long TimestampMs,
    string Kind,
    string Summary,
    string TimeText);
