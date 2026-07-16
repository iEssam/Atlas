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
    [ObservableProperty] private string _recencyText = "Connecting to Atlas Service";
    [ObservableProperty] private bool _isDisconnected;
    [ObservableProperty] private bool _isTraceLoading;
    [ObservableProperty] private bool _isTraceUnavailable;
    [ObservableProperty] private bool _isTraceEmpty;
    [ObservableProperty] private bool _hasEvidence;
    [ObservableProperty] private bool _isEvidenceLoading = true;
    [ObservableProperty] private bool _isEvidenceClear;
    [ObservableProperty] private string _traceStatus = "Waiting for the first measured sample.";
    [ObservableProperty] private string _traceAutomationSummary = "System history has not loaded yet.";
    [ObservableProperty] private string _evidenceStatus = "Loading recent evidence.";
    [ObservableProperty] private string _gpuHistoryLabel = "GPU";

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
    public string CountText => $"{ThreadCount:N0} threads · {HandleCount:N0} handles";
    public long TraceFromMs { get; private set; }
    public long TraceToMs { get; private set; }

    public event Action? TraceRefreshed;

    public OverviewViewModel(DispatcherQueue dispatcher, string? who = null)
    {
        _dispatcher = dispatcher;
        _who = who;
        _metrics = new LiveMetricsService(dispatcher, who);
        _metrics.StatusChanged += OnStatusChanged;
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
        _traceCts?.Cancel();
        var cts = new CancellationTokenSource();
        _traceCts = cts;
        var ct = cts.Token;
        _traceRequested = true;
        IsTraceLoading = true;
        IsTraceUnavailable = false;
        IsTraceEmpty = false;
        IsEvidenceLoading = true;
        IsEvidenceClear = false;
        EvidenceStatus = "Loading recent evidence.";
        TraceStatus = "Loading the last 15 minutes of measured evidence.";
        TraceAutomationSummary = "Loading the fifteen minute system trace.";

        var now = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
        var from = now - (long)TimeSpan.FromMinutes(15).TotalMilliseconds;
        TraceFromMs = from;
        TraceToMs = now;

        try
        {
            using var channel = AtlasChannel.Connect(_who);
            // Start every independent request together, but observe each through
            // an isolated result. One old or failing endpoint must never erase
            // data returned by the other five endpoints.
            var cpuTask = FetchAsync(
                () => channel.QueryRangeAsync(MetricKind.SysCpuPermille, 0, from, now, 180, ct), ct);
            var memoryTask = FetchAsync(
                () => channel.QueryRangeAsync(MetricKind.SysMemUsed, 0, from, now, 180, ct), ct);
            var gpuTask = FetchAsync(
                () => channel.QueryRangeAsync(MetricKind.SysGpuPermille, 0, from, now, 180, ct), ct);
            var incidentTask = FetchAsync(() => channel.ListIncidentsAsync(from, now, 12, ct), ct);
            var privacyTask = FetchAsync(() => channel.ListPrivacyEventsAsync(from, now, 12, ct), ct);
            var changesTask = FetchAsync(
                () => channel.ListSystemChangesAsync(from, now, null, 12, ct), ct);

            var cpu = await cpuTask.ConfigureAwait(false);
            var memory = await memoryTask.ConfigureAwait(false);
            var gpu = await gpuTask.ConfigureAwait(false);
            var incidents = await incidentTask.ConfigureAwait(false);
            var privacy = await privacyTask.ConfigureAwait(false);
            var changes = await changesTask.ConfigureAwait(false);
            if (ct.IsCancellationRequested)
            {
                return;
            }

            var totalBytes = MemTotalGb * 1024.0 * 1024.0 * 1024.0;
            var cpuPoints = cpu.HasValue
                ? ToPercentPoints(cpu.Value!.Buckets, 10.0)
                : [];
            var memoryPoints = memory.HasValue
                ? ToPercentPoints(memory.Value!.Buckets, totalBytes / 100.0)
                : [];
            var gpuPoints = gpu.HasValue
                ? ToPercentPoints(gpu.Value!.Buckets, 10.0)
                : [];
            var evidence = new List<OverviewEvidenceMarker>();

            if (incidents.HasValue)
            {
                evidence.AddRange(incidents.Value!.Incidents.Select(item => new OverviewEvidenceMarker(
                    item.StartMs,
                    "Incident",
                    item.Summary,
                    FormatTime(item.StartMs))));
            }
            if (privacy.HasValue)
            {
                evidence.AddRange(privacy.Value!.Events.Select(item => new OverviewEvidenceMarker(
                    item.TsMs,
                    "Privacy",
                    $"{item.DisplayName}: {item.Capability} {(item.Started ? "started" : "stopped")}",
                    FormatTime(item.TsMs))));
            }
            if (changes.HasValue)
            {
                evidence.AddRange(changes.Value!.Changes.Select(item => new OverviewEvidenceMarker(
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
                HasEvidence = Evidence.Count > 0;
                IsEvidenceLoading = false;

                var metricResults = new[] { cpu, memory, gpu };
                var availableMetricCount = metricResults.Count(result => result.HasValue);
                var failedMetricCount = metricResults.Count(result => result.IsFaulted);
                var pointCount = cpuPoints.Count + memoryPoints.Count + gpuPoints.Count;
                IsTraceUnavailable = availableMetricCount == 0;
                IsTraceEmpty = availableMetricCount > 0 && pointCount == 0;
                GpuHistoryLabel = gpu.HasValue ? "GPU · dashed" : "GPU unavailable";

                TraceStatus = IsTraceUnavailable
                    ? (failedMetricCount > 0
                        ? "Could not reach the historical data service."
                        : "Historical trace is not supported by the connected service.")
                    : IsTraceEmpty
                        ? "Recording has just started."
                        : availableMetricCount == 3
                            ? "Measured averages with recorded minimum and maximum ranges."
                            : $"{availableMetricCount} of 3 history series available.";

                EvidenceStatus = Evidence.Count > 0
                    ? $"{Evidence.Count} recent evidence item{(Evidence.Count == 1 ? "" : "s")}."
                    : incidents.HasValue
                        ? "No active incidents in the last 15 minutes."
                        : incidents.IsFaulted
                            ? "Recent incident evidence could not be loaded."
                            : "Recent incident evidence is not supported by the connected service.";
                IsEvidenceClear = Evidence.Count == 0 && incidents.HasValue;

                TraceAutomationSummary = BuildTraceAutomationSummary(
                    cpuPoints, memoryPoints, gpuPoints, TraceStatus);
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
                IsTraceEmpty = false;
                TraceStatus = $"Could not load historical evidence: {ex.Message}";
                TraceAutomationSummary = TraceStatus;
                IsEvidenceLoading = false;
                IsEvidenceClear = false;
                HasEvidence = false;
                EvidenceStatus = "Recent evidence could not be loaded.";
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
        IsDisconnected = false;
        RecencyText = "Updated just now";

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
                Math.Clamp(bucket.Min / divisor, 0, 100),
                Math.Clamp(bucket.Max / divisor, 0, 100),
                Math.Clamp(bucket.Avg / divisor, 0, 100)))
            .ToList();
    }

    private void OnStatusChanged(MetricsSource source, string status)
    {
        ConnectionStatus = status;
        var connecting = status.StartsWith("Connecting", StringComparison.OrdinalIgnoreCase);
        var connected = status.StartsWith("Connected", StringComparison.OrdinalIgnoreCase);
        IsDisconnected = !connecting && !connected;
        if (IsDisconnected)
        {
            RecencyText = "Service disconnected";
        }
        else if (connecting)
        {
            RecencyText = "Connecting to Atlas Service";
        }
    }

    private static async Task<FetchResult<T>> FetchAsync<T>(
        Func<Task<RpcOutcome<T>>> fetch,
        CancellationToken cancellationToken)
    {
        try
        {
            var outcome = await fetch().ConfigureAwait(false);
            return outcome.Supported
                ? FetchResult<T>.Success(outcome.Value)
                : FetchResult<T>.Unsupported(outcome.UnsupportedReason);
        }
        catch (OperationCanceledException) when (cancellationToken.IsCancellationRequested)
        {
            throw;
        }
        catch (Exception ex)
        {
            return FetchResult<T>.Failure(ex.Message);
        }
    }

    private static string BuildTraceAutomationSummary(
        IReadOnlyCollection<OverviewTracePoint> cpu,
        IReadOnlyCollection<OverviewTracePoint> memory,
        IReadOnlyCollection<OverviewTracePoint> gpu,
        string fallback)
    {
        var parts = new List<string>();
        AddSummary(parts, "CPU", cpu);
        AddSummary(parts, "memory", memory);
        AddSummary(parts, "GPU", gpu);
        return parts.Count == 0
            ? fallback
            : "Last fifteen minutes. " + string.Join(" ", parts);
    }

    private static void AddSummary(
        ICollection<string> parts,
        string label,
        IReadOnlyCollection<OverviewTracePoint> points)
    {
        if (points.Count == 0)
        {
            return;
        }

        parts.Add($"{label} average {points.Average(point => point.AveragePercent):F0} percent, " +
            $"peak {points.Max(point => point.MaxPercent):F0} percent.");
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

public readonly record struct OverviewTracePoint(
    long TimestampMs,
    double MinPercent,
    double MaxPercent,
    double AveragePercent);

internal readonly record struct FetchResult<T>(
    T? Value,
    bool IsSupported,
    bool IsFaulted,
    string? Error)
{
    public bool HasValue => IsSupported && Value is not null;

    public static FetchResult<T> Success(T value) => new(value, true, false, null);
    public static FetchResult<T> Unsupported(string? reason) =>
        new(default, false, false, reason ?? "Unsupported");
    public static FetchResult<T> Failure(string error) => new(default, false, true, error);
}

public sealed record OverviewEvidenceMarker(
    long TimestampMs,
    string Kind,
    string Summary,
    string TimeText)
{
    public string AutomationName => $"{Kind}: {Summary}, {TimeText}. Open related evidence.";
}
