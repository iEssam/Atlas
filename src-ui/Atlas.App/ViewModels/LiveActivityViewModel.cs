using System.Collections.ObjectModel;
using Atlas.App.Models;
using Atlas.App.Services;
using Atlas.IpcClient;
using CommunityToolkit.Mvvm.ComponentModel;
using Microsoft.UI.Dispatching;

namespace Atlas.App.ViewModels;

public enum ActivityViewMode
{
    All,
    Tracked,
    Changes,
}

public enum ProcessSortMode
{
    Name,
    Pid,
    Cpu,
    AverageCpu,
    Gpu,
    WorkingSet,
    MemoryDelta,
    Disk,
}

/// <summary>
/// Drives the Activity watchboard. This page deliberately uses the complete
/// service stream instead of the 64-row shared-memory leaderboard: tracking
/// needs stable process identity and must not mistake falling out of the ring's
/// top set for a process exit.
/// </summary>
public sealed partial class LiveActivityViewModel : ObservableObject
{
    private const double CpuChangeThreshold = 25;
    private const double MemoryGrowthThresholdMb = 256;
    private const int ChangeLimit = 250;

    private readonly DispatcherQueue _dispatcher;
    private readonly LiveMetricsService _metrics;
    private readonly IUiPreferencesStore _preferences;
    private readonly string? _who;
    private readonly Dictionary<ProcessIdentity, ProcessRowViewModel> _liveIndex = new();
    private readonly List<ProcessRowViewModel> _liveOrder = new();
    private readonly List<ProcessRowViewModel> _endedTracked = new();
    private readonly HashSet<string> _trackedApplications;
    private readonly HashSet<ProcessIdentity> _cpuAlerts = new();
    private readonly HashSet<ProcessIdentity> _memoryAlerts = new();
    private readonly Queue<SystemLiveSample> _systemSamples = new();

    private CancellationTokenSource? _cts;
    private MetricsSnapshot? _pendingSnapshot;
    private bool _hasInitialSnapshot;
    private string _searchText = string.Empty;
    private ActivityViewMode _viewMode;
    private ProcessSortMode _sortMode = ProcessSortMode.Name;
    private bool _sortDescending;

    public ObservableCollection<ProcessRowViewModel> Processes { get; } = new();
    public ObservableCollection<ActivityChangeViewModel> Changes { get; } = new();

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(TrackActionText))]
    [NotifyPropertyChangedFor(nameof(CanTrackSelection))]
    [NotifyPropertyChangedFor(nameof(CanInspectSelection))]
    private ProcessRowViewModel? _selectedProcess;

    [ObservableProperty]
    private string _connectionStatus = "Connecting...";

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(ConnectionSourceText))]
    [NotifyPropertyChangedFor(nameof(IsConnected))]
    [NotifyPropertyChangedFor(nameof(ShowConnectionWarning))]
    private MetricsSource _source;

    [ObservableProperty]
    private string _serviceVersion = "-";

    [ObservableProperty]
    private string _capabilityFlags = "-";

    [ObservableProperty]
    private double _systemCpuPercent;

    [ObservableProperty]
    private double _systemGpuPercent;

    [ObservableProperty]
    private double _memUsedGb;

    [ObservableProperty]
    private double _memTotalGb;

    [ObservableProperty]
    private uint _processCount;

    [ObservableProperty]
    private uint _threadCount;

    [ObservableProperty]
    private uint _handleCount;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(PauseActionText))]
    [NotifyPropertyChangedFor(nameof(PauseActionGlyph))]
    private bool _isPaused;

    [ObservableProperty]
    private bool _hasSnapshot;

    [ObservableProperty]
    private bool _hasVisibleProcesses;

    [ObservableProperty]
    private bool _hasChanges;

    [ObservableProperty]
    private string _visibleCountText = "Waiting for the first snapshot";

    [ObservableProperty]
    private string _systemInsightText = "Waiting for measured changes.";

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(HasInteractionMessage))]
    private string _interactionMessage = string.Empty;

    public ActivityViewMode ViewMode => _viewMode;
    public ProcessSortMode SortMode => _sortMode;
    public bool SortDescending => _sortDescending;
    public bool IsConnected => Source is MetricsSource.Ring or MetricsSource.Stream;
    public bool ShowConnectionWarning => !IsConnected && !ConnectionStatus.StartsWith("Connecting", StringComparison.OrdinalIgnoreCase);
    public bool HasInteractionMessage => !string.IsNullOrWhiteSpace(InteractionMessage);
    public bool CanTrackSelection => SelectedProcess is not null;
    public bool CanInspectSelection => SelectedProcess?.IsRunning == true;
    public string TrackActionText => SelectedProcess?.TrackActionText ?? "Track application";
    public string PauseActionText => IsPaused ? "Resume" : "Pause";
    public string PauseActionGlyph => IsPaused ? "\uE768" : "\uE769";
    public string ConnectionSourceText => Source switch
    {
        MetricsSource.Stream => "Full service stream",
        MetricsSource.Ring => "Shared-memory ring",
        _ => "No live source",
    };
    public string EmptyTitle => !HasSnapshot
        ? "Connecting to live activity"
        : _viewMode == ActivityViewMode.Tracked
            ? "Nothing tracked yet"
            : "No matching processes";
    public string EmptyMessage => !HasSnapshot
        ? "The process list will appear after the service publishes its first complete snapshot."
        : _viewMode == ActivityViewMode.Tracked
            ? "Select a process in All processes, then choose Track application."
            : string.IsNullOrWhiteSpace(_searchText)
                ? "The service returned no live processes."
                : $"No process name or PID contains ‘{_searchText}’.";

    public string NameHeader => Header("Name", ProcessSortMode.Name);
    public string PidHeader => Header("PID", ProcessSortMode.Pid);
    public string CpuHeader => Header("CPU %", ProcessSortMode.Cpu);
    public string AverageCpuHeader => Header("1m avg", ProcessSortMode.AverageCpu);
    public string GpuHeader => Header("GPU %", ProcessSortMode.Gpu);
    public string MemoryHeader => Header("Memory MB", ProcessSortMode.WorkingSet);
    public string MemoryDeltaHeader => Header("1m change", ProcessSortMode.MemoryDelta);
    public string DiskHeader => Header("Disk MB/s", ProcessSortMode.Disk);

    public LiveActivityViewModel(
        DispatcherQueue dispatcher,
        IUiPreferencesStore preferences,
        string? who = null)
    {
        _dispatcher = dispatcher;
        _preferences = preferences;
        _who = who;
        _trackedApplications = new HashSet<string>(
            preferences.Current.TrackedApplications,
            StringComparer.OrdinalIgnoreCase);

        // The Overview and shell keep the low-overhead ring. Activity needs all
        // processes plus create-time/thread/handle metadata, so it opts into the
        // complete stream explicitly.
        _metrics = new LiveMetricsService(dispatcher, who, preferRing: false);
        _metrics.StatusChanged += OnStatusChanged;
        _metrics.SnapshotReceived += Apply;
    }

    public void Start()
    {
        if (_cts is not null)
        {
            return;
        }

        _cts = new CancellationTokenSource();
        _ = FetchCapabilitiesAsync(_cts.Token);
        _metrics.Start();
    }

    public void Stop()
    {
        _cts?.Cancel();
        _cts?.Dispose();
        _cts = null;
        _metrics.Stop();
    }

    public void SetViewMode(ActivityViewMode mode)
    {
        if (_viewMode == mode)
        {
            return;
        }

        _viewMode = mode;
        OnPropertyChanged(nameof(ViewMode));
        RefreshVisibleProcesses();
    }

    public void SetSearchText(string text)
    {
        string next = text?.Trim() ?? string.Empty;
        if (string.Equals(_searchText, next, StringComparison.OrdinalIgnoreCase))
        {
            return;
        }

        _searchText = next;
        RefreshVisibleProcesses();
    }

    public void SortBy(ProcessSortMode mode)
    {
        if (_sortMode == mode)
        {
            _sortDescending = !_sortDescending;
        }
        else
        {
            _sortMode = mode;
            _sortDescending = mode is not ProcessSortMode.Name and not ProcessSortMode.Pid;
        }

        SortCurrentSnapshot();
        RaiseHeaderChanges();
        RefreshVisibleProcesses();
    }

    public void TogglePause()
    {
        IsPaused = !IsPaused;
        if (!IsPaused && _pendingSnapshot is { } pending)
        {
            _pendingSnapshot = null;
            ApplyCore(pending);
        }
    }

    public async Task ToggleSelectedTrackingAsync()
    {
        if (SelectedProcess is { } selected)
        {
            await ToggleTrackingAsync(selected);
        }
    }

    public async Task ToggleTrackingAsync(ProcessRowViewModel row)
    {
        string application = NormalizeApplication(row.ImageName);
        if (application.Length == 0)
        {
            return;
        }

        bool tracked;
        if (_trackedApplications.Contains(application))
        {
            _trackedApplications.Remove(application);
            tracked = false;
        }
        else
        {
            _trackedApplications.Add(application);
            tracked = true;
        }

        foreach (var process in _liveOrder.Where(process =>
                     string.Equals(NormalizeApplication(process.ImageName), application, StringComparison.OrdinalIgnoreCase)))
        {
            process.IsTracked = tracked;
        }

        if (!tracked)
        {
            _endedTracked.RemoveAll(process =>
                string.Equals(NormalizeApplication(process.ImageName), application, StringComparison.OrdinalIgnoreCase));
        }

        AddChange(new ActivityChangeViewModel(
            DateTimeOffset.Now,
            tracked ? ActivityChangeKind.TrackingStarted : ActivityChangeKind.TrackingStopped,
            tracked ? $"Tracking {row.ImageName}" : $"Stopped tracking {row.ImageName}",
            tracked
                ? "New instances of this application will be followed across restarts."
                : "Existing samples remain in history, but new instances will not be followed.",
            tracked));

        RefreshVisibleProcesses();
        OnPropertyChanged(nameof(TrackActionText));

        var preferences = _preferences.Current;
        preferences.TrackedApplications = _trackedApplications
            .OrderBy(name => name, StringComparer.OrdinalIgnoreCase)
            .ToList();

        try
        {
            await _preferences.SaveAsync(preferences);
            InteractionMessage = tracked
                ? $"Tracking {row.ImageName} across restarts."
                : $"Stopped tracking {row.ImageName}.";
        }
        catch (Exception ex)
        {
            InteractionMessage = $"Tracking changed for this session, but could not be saved: {ex.Message}";
        }
    }

    public void DismissInteractionMessage() => InteractionMessage = string.Empty;

    private void OnStatusChanged(MetricsSource source, string status)
    {
        Source = source;
        ConnectionStatus = status;
        OnPropertyChanged(nameof(EmptyTitle));
        OnPropertyChanged(nameof(EmptyMessage));
    }

    private async Task FetchCapabilitiesAsync(CancellationToken cancellationToken)
    {
        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var capabilities = await channel.GetCapabilitiesAsync(cancellationToken).ConfigureAwait(false);
            Post(() =>
            {
                ServiceVersion = capabilities.ServiceVersion;
                CapabilityFlags = string.Join(", ", capabilities.CapabilityFlags);
            });
        }
        catch
        {
            // The service details flyout is informational. Live metrics continue
            // even when this best-effort metadata request is unavailable.
        }
    }

    private void Apply(MetricsSnapshot snapshot)
    {
        if (IsPaused)
        {
            _pendingSnapshot = snapshot;
            return;
        }

        ApplyCore(snapshot);
    }

    private void ApplyCore(MetricsSnapshot snapshot)
    {
        long timestampMs = snapshot.TimestampMs > 0
            ? snapshot.TimestampMs
            : DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();

        SystemCpuPercent = snapshot.CpuPercent;
        SystemGpuPercent = snapshot.GpuPercent;
        MemUsedGb = snapshot.MemUsed / (1024.0 * 1024.0 * 1024.0);
        MemTotalGb = snapshot.MemTotal / (1024.0 * 1024.0 * 1024.0);
        ProcessCount = snapshot.ProcessCount;
        ThreadCount = snapshot.ThreadCount;
        HandleCount = snapshot.HandleCount;
        HasSnapshot = true;

        UpdateSystemInsight(snapshot, timestampMs);

        var seen = new HashSet<ProcessIdentity>(snapshot.Rows.Count);
        bool structureChanged = false;

        foreach (var row in snapshot.Rows)
        {
            var identity = new ProcessIdentity(row.Pid, row.CreateTime100ns);
            seen.Add(identity);

            bool isNew = !_liveIndex.TryGetValue(identity, out var process);
            if (isNew)
            {
                process = new ProcessRowViewModel(row.Pid, row.CreateTime100ns)
                {
                    IsTracked = _trackedApplications.Contains(NormalizeApplication(row.ImageName)),
                };
                _liveIndex.Add(identity, process);
                _liveOrder.Add(process);
                structureChanged = true;
            }

            bool hadSamples = process!.LastSeenMs > 0;
            bool wasCpuElevated = process.CpuPercent >= CpuChangeThreshold;
            bool wasMemoryGrowing = process.MemoryDeltaOneMinuteMb >= MemoryGrowthThresholdMb;
            process.Update(row, timestampMs, snapshot.Source == MetricsSource.Stream);

            if (_hasInitialSnapshot && isNew)
            {
                AddChange(new ActivityChangeViewModel(
                    DateTimeOffset.FromUnixTimeMilliseconds(timestampMs),
                    ActivityChangeKind.Started,
                    $"{process.ImageName} started",
                    $"PID {process.Pid}",
                    process.IsTracked));
            }

            if (_hasInitialSnapshot && hadSamples && process.IsTracked)
            {
                UpdateMeasuredChangeEvents(process, wasCpuElevated, wasMemoryGrowing, timestampMs);
            }
        }

        for (int index = _liveOrder.Count - 1; index >= 0; index--)
        {
            var process = _liveOrder[index];
            if (seen.Contains(process.Identity))
            {
                continue;
            }

            process.MarkEnded(timestampMs);
            if (ReferenceEquals(SelectedProcess, process))
            {
                OnPropertyChanged(nameof(CanInspectSelection));
            }
            _liveOrder.RemoveAt(index);
            _liveIndex.Remove(process.Identity);
            _cpuAlerts.Remove(process.Identity);
            _memoryAlerts.Remove(process.Identity);
            structureChanged = true;

            if (process.IsTracked)
            {
                _endedTracked.Insert(0, process);
                if (_endedTracked.Count > 50)
                {
                    _endedTracked.RemoveAt(_endedTracked.Count - 1);
                }
            }

            if (_hasInitialSnapshot)
            {
                AddChange(new ActivityChangeViewModel(
                    DateTimeOffset.FromUnixTimeMilliseconds(timestampMs),
                    ActivityChangeKind.Ended,
                    $"{process.ImageName} ended",
                    $"PID {process.Pid}; last CPU {process.CpuText}%",
                    process.IsTracked));
            }
        }

        if (!_hasInitialSnapshot)
        {
            SortCurrentSnapshot();
            structureChanged = true;
            _hasInitialSnapshot = true;
        }

        if (structureChanged)
        {
            RefreshVisibleProcesses();
        }
        else
        {
            UpdateVisibleCount();
        }
    }

    private void UpdateMeasuredChangeEvents(
        ProcessRowViewModel process,
        bool wasCpuElevated,
        bool wasMemoryGrowing,
        long timestampMs)
    {
        bool cpuElevated = process.CpuPercent >= CpuChangeThreshold;
        if (!wasCpuElevated && cpuElevated && _cpuAlerts.Add(process.Identity))
        {
            AddChange(new ActivityChangeViewModel(
                DateTimeOffset.FromUnixTimeMilliseconds(timestampMs),
                ActivityChangeKind.CpuRaised,
                $"{process.ImageName} CPU rose",
                $"{process.CpuText}% now; {process.AverageCpuText}% one-minute average",
                true));
        }
        else if (wasCpuElevated && !cpuElevated && _cpuAlerts.Remove(process.Identity))
        {
            AddChange(new ActivityChangeViewModel(
                DateTimeOffset.FromUnixTimeMilliseconds(timestampMs),
                ActivityChangeKind.CpuSettled,
                $"{process.ImageName} CPU settled",
                $"{process.CpuText}% now after exceeding {CpuChangeThreshold:F0}%",
                true));
        }

        bool memoryGrowing = process.MemoryDeltaOneMinuteMb >= MemoryGrowthThresholdMb;
        if (!wasMemoryGrowing && memoryGrowing && _memoryAlerts.Add(process.Identity))
        {
            AddChange(new ActivityChangeViewModel(
                DateTimeOffset.FromUnixTimeMilliseconds(timestampMs),
                ActivityChangeKind.MemoryGrowth,
                $"{process.ImageName} memory increased",
                $"{process.MemoryDeltaText} over the measured one-minute window",
                true));
        }
        else if (wasMemoryGrowing && !memoryGrowing)
        {
            _memoryAlerts.Remove(process.Identity);
        }
    }

    private void UpdateSystemInsight(MetricsSnapshot snapshot, long timestampMs)
    {
        _systemSamples.Enqueue(new SystemLiveSample(timestampMs, snapshot.CpuPercent, MemUsedGb));
        long cutoff = timestampMs - (long)TimeSpan.FromMinutes(1).TotalMilliseconds;
        while (_systemSamples.Count > 1 && _systemSamples.Peek().TimestampMs < cutoff)
        {
            _systemSamples.Dequeue();
        }

        var baseline = _systemSamples.Peek();
        double cpuDelta = snapshot.CpuPercent - baseline.CpuPercent;
        double memoryDelta = MemUsedGb - baseline.MemoryUsedGb;
        int elevatedProcesses = snapshot.Rows.Count(row => row.CpuPercent >= CpuChangeThreshold);

        string cpuPhrase = Math.Abs(cpuDelta) < 0.5
            ? "CPU is stable"
            : cpuDelta > 0
                ? $"CPU rose {cpuDelta:F1} points"
                : $"CPU fell {Math.Abs(cpuDelta):F1} points";
        string memoryPhrase = Math.Abs(memoryDelta) < 0.05
            ? "memory is stable"
            : memoryDelta > 0
                ? $"memory increased {memoryDelta:F1} GB"
                : $"memory decreased {Math.Abs(memoryDelta):F1} GB";
        string processPhrase = elevatedProcesses == 0
            ? $"no process is above {CpuChangeThreshold:F0}% CPU"
            : elevatedProcesses == 1
                ? $"1 process is above {CpuChangeThreshold:F0}% CPU"
                : $"{elevatedProcesses} processes are above {CpuChangeThreshold:F0}% CPU";

        double elapsedSeconds = Math.Max(1, (timestampMs - baseline.TimestampMs) / 1000.0);
        string windowPhrase = elapsedSeconds >= 55
            ? "Over the last minute of samples"
            : $"Across {elapsedSeconds:F0} seconds of samples";
        SystemInsightText = $"{windowPhrase}: {cpuPhrase}; {memoryPhrase}; {processPhrase}.";
    }

    private void AddChange(ActivityChangeViewModel change)
    {
        Changes.Insert(0, change);
        while (Changes.Count > ChangeLimit)
        {
            Changes.RemoveAt(Changes.Count - 1);
        }

        HasChanges = Changes.Count > 0;
        if (_viewMode == ActivityViewMode.Changes)
        {
            UpdateVisibleCount();
        }
    }

    private void SortCurrentSnapshot()
    {
        IOrderedEnumerable<ProcessRowViewModel> ordered = (_sortMode, _sortDescending) switch
        {
            (ProcessSortMode.Name, false) => _liveOrder.OrderBy(p => p.ImageName, StringComparer.OrdinalIgnoreCase),
            (ProcessSortMode.Name, true) => _liveOrder.OrderByDescending(p => p.ImageName, StringComparer.OrdinalIgnoreCase),
            (ProcessSortMode.Pid, false) => _liveOrder.OrderBy(p => p.Pid),
            (ProcessSortMode.Pid, true) => _liveOrder.OrderByDescending(p => p.Pid),
            (ProcessSortMode.Cpu, false) => _liveOrder.OrderBy(p => p.CpuPercent),
            (ProcessSortMode.Cpu, true) => _liveOrder.OrderByDescending(p => p.CpuPercent),
            (ProcessSortMode.AverageCpu, false) => _liveOrder.OrderBy(p => p.AverageCpuOneMinute),
            (ProcessSortMode.AverageCpu, true) => _liveOrder.OrderByDescending(p => p.AverageCpuOneMinute),
            (ProcessSortMode.Gpu, false) => _liveOrder.OrderBy(p => p.GpuPercent),
            (ProcessSortMode.Gpu, true) => _liveOrder.OrderByDescending(p => p.GpuPercent),
            (ProcessSortMode.WorkingSet, false) => _liveOrder.OrderBy(p => p.WorkingSetMb),
            (ProcessSortMode.WorkingSet, true) => _liveOrder.OrderByDescending(p => p.WorkingSetMb),
            (ProcessSortMode.MemoryDelta, false) => _liveOrder.OrderBy(p => p.MemoryDeltaOneMinuteMb),
            (ProcessSortMode.MemoryDelta, true) => _liveOrder.OrderByDescending(p => p.MemoryDeltaOneMinuteMb),
            (ProcessSortMode.Disk, false) => _liveOrder.OrderBy(p => p.DiskMbPerSecond),
            _ => _liveOrder.OrderByDescending(p => p.DiskMbPerSecond),
        };

        var snapshot = ordered
            .ThenBy(process => process.ImageName, StringComparer.OrdinalIgnoreCase)
            .ThenBy(process => process.Pid)
            .ToArray();
        _liveOrder.Clear();
        _liveOrder.AddRange(snapshot);
    }

    private void RefreshVisibleProcesses()
    {
        var selected = SelectedProcess;
        IEnumerable<ProcessRowViewModel> source = _viewMode == ActivityViewMode.Tracked
            ? _liveOrder.Where(process => process.IsTracked).Concat(_endedTracked)
            : _liveOrder;

        if (!string.IsNullOrWhiteSpace(_searchText))
        {
            source = source.Where(process =>
                process.ImageName.Contains(_searchText, StringComparison.OrdinalIgnoreCase)
                || process.Pid.ToString().Contains(_searchText, StringComparison.OrdinalIgnoreCase)
                || process.AppGroup.Contains(_searchText, StringComparison.OrdinalIgnoreCase));
        }

        Processes.Clear();
        foreach (var process in source)
        {
            Processes.Add(process);
        }

        HasVisibleProcesses = Processes.Count > 0;
        if (selected is not null && Processes.Contains(selected))
        {
            SelectedProcess = selected;
        }
        else if (_viewMode != ActivityViewMode.Changes)
        {
            SelectedProcess = null;
        }

        UpdateVisibleCount();
        OnPropertyChanged(nameof(EmptyTitle));
        OnPropertyChanged(nameof(EmptyMessage));
    }

    private void UpdateVisibleCount()
    {
        VisibleCountText = _viewMode switch
        {
            ActivityViewMode.Tracked => $"{Processes.Count} tracked instance{(Processes.Count == 1 ? string.Empty : "s")}",
            ActivityViewMode.Changes => $"{Changes.Count} measured change{(Changes.Count == 1 ? string.Empty : "s")}",
            _ => $"{Processes.Count} shown of {_liveOrder.Count} live processes",
        };
    }

    private string Header(string label, ProcessSortMode mode) =>
        _sortMode == mode ? $"{label} {(_sortDescending ? "↓" : "↑")}" : label;

    private void RaiseHeaderChanges()
    {
        OnPropertyChanged(nameof(NameHeader));
        OnPropertyChanged(nameof(PidHeader));
        OnPropertyChanged(nameof(CpuHeader));
        OnPropertyChanged(nameof(AverageCpuHeader));
        OnPropertyChanged(nameof(GpuHeader));
        OnPropertyChanged(nameof(MemoryHeader));
        OnPropertyChanged(nameof(MemoryDeltaHeader));
        OnPropertyChanged(nameof(DiskHeader));
        OnPropertyChanged(nameof(SortMode));
        OnPropertyChanged(nameof(SortDescending));
    }

    private static string NormalizeApplication(string imageName) => imageName.Trim();

    private void Post(Action action) => _dispatcher.TryEnqueue(() => action());

    private readonly record struct SystemLiveSample(long TimestampMs, double CpuPercent, double MemoryUsedGb);
}

public enum ActivityChangeKind
{
    Started,
    Ended,
    CpuRaised,
    CpuSettled,
    MemoryGrowth,
    TrackingStarted,
    TrackingStopped,
}

/// <summary>One immutable entry in the measured Changes feed.</summary>
public sealed class ActivityChangeViewModel
{
    public DateTimeOffset Timestamp { get; }
    public ActivityChangeKind Kind { get; }
    public string Title { get; }
    public string Detail { get; }
    public bool IsTracked { get; }
    public string TimeText => Timestamp.ToString("t");
    public string KindText => Kind switch
    {
        ActivityChangeKind.Started => "Started",
        ActivityChangeKind.Ended => "Ended",
        ActivityChangeKind.CpuRaised => "CPU increase",
        ActivityChangeKind.CpuSettled => "CPU settled",
        ActivityChangeKind.MemoryGrowth => "Memory increase",
        ActivityChangeKind.TrackingStarted => "Tracking started",
        _ => "Tracking stopped",
    };
    public string AccessibleSummary => $"{TimeText}, {KindText}: {Title}. {Detail}";

    public override string ToString() => AccessibleSummary;
    public string IconGlyph => Kind switch
    {
        ActivityChangeKind.Started => "\uE74A",
        ActivityChangeKind.Ended => "\uE711",
        ActivityChangeKind.CpuRaised => "\uE9D9",
        ActivityChangeKind.CpuSettled => "\uE73E",
        ActivityChangeKind.MemoryGrowth => "\uE8C8",
        ActivityChangeKind.TrackingStarted => "\uE735",
        _ => "\uE734",
    };

    public ActivityChangeViewModel(
        DateTimeOffset timestamp,
        ActivityChangeKind kind,
        string title,
        string detail,
        bool isTracked)
    {
        Timestamp = timestamp.ToLocalTime();
        Kind = kind;
        Title = title;
        Detail = detail;
        IsTracked = isTracked;
    }
}
