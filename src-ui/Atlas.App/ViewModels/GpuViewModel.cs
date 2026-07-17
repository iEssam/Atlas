using System.Collections.ObjectModel;
using System.Globalization;
using Atlas.App.Models;
using Atlas.App.Services;
using Atlas.IpcClient;
using Atlas.V0;
using CommunityToolkit.Mvvm.ComponentModel;
using Microsoft.UI.Dispatching;

namespace Atlas.App.ViewModels;

public enum GpuViewMode
{
    All,
    Tracked,
    Changes,
}

public enum GpuProcessSortMode
{
    Name,
    Pid,
    Gpu,
    AverageGpu,
    Dedicated,
    DedicatedDelta,
    Shared,
}

/// <summary>
/// Stable, adapter-aware GPU watchboard. Detailed adapter telemetry and the
/// complete process set come from the service stream because the shared-memory
/// ring intentionally carries only system aggregates and a short leaderboard.
/// </summary>
public sealed partial class GpuViewModel : ObservableObject
{
    private const double HighGpuThreshold = 80;
    private const double HighGpuSettleThreshold = 65;
    private const double ProcessGpuThreshold = 25;
    private const double ProcessGpuSettleThreshold = 15;
    private const double MemoryPressureThreshold = 85;
    private const double MemorySettleThreshold = 75;
    private const double TemperatureMarginThreshold = 5;
    private const double TemperatureSettleMargin = 10;
    private const int ChangeLimit = 250;

    private readonly DispatcherQueue _dispatcher;
    private readonly IUiPreferencesStore _preferences;
    private readonly string? _who;
    private readonly Dictionary<ProcessIdentity, GpuProcessItem> _processIndex = new();
    private readonly List<GpuProcessItem> _processOrder = new();
    private readonly List<GpuProcessItem> _endedTracked = new();
    private readonly HashSet<string> _trackedApplications;

    private CancellationTokenSource? _cts;
    private SnapshotReply? _pendingSnapshot;
    private bool _hasInitialSnapshot;
    private string _searchText = string.Empty;
    private GpuViewMode _viewMode;
    private GpuProcessSortMode _sortMode = GpuProcessSortMode.Gpu;
    private bool _sortDescending = true;

    public ObservableCollection<GpuAdapterItem> Adapters { get; } = new();
    public ObservableCollection<GpuProcessItem> Processes { get; } = new();
    public ObservableCollection<GpuChangeViewModel> Changes { get; } = new();

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(HasAdapter))]
    private GpuAdapterItem? _selectedAdapter;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(CanTrackSelection))]
    [NotifyPropertyChangedFor(nameof(CanInspectSelection))]
    [NotifyPropertyChangedFor(nameof(TrackActionText))]
    private GpuProcessItem? _selectedProcess;

    [ObservableProperty]
    private string _statusText = "Connecting to graphics telemetry";

    [ObservableProperty]
    private bool _isUnavailable;

    [ObservableProperty]
    private string _unavailableReason = string.Empty;

    [ObservableProperty]
    private bool _hasSnapshot;

    [ObservableProperty]
    private bool _hasVisibleProcesses;

    [ObservableProperty]
    private bool _hasChanges;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(PauseActionText))]
    [NotifyPropertyChangedFor(nameof(PauseActionGlyph))]
    private bool _isPaused;

    [ObservableProperty]
    private string _visibleCountText = "Waiting for the first snapshot";

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(GraphicsInsightSummary))]
    private string _graphicsStateTitle = "Measuring graphics behavior";

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(GraphicsInsightSummary))]
    private string _graphicsInsightText = "A factual interpretation will appear after live samples arrive.";

    [ObservableProperty]
    private string _topProcessText = "No measured process";

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(HasInteractionMessage))]
    private string _interactionMessage = string.Empty;

    public GpuViewMode ViewMode => _viewMode;
    public GpuProcessSortMode SortMode => _sortMode;
    public bool SortDescending => _sortDescending;
    public bool HasAdapter => SelectedAdapter is not null;
    public bool CanTrackSelection => SelectedProcess is not null;
    public bool CanInspectSelection => SelectedProcess?.IsRunning == true;
    public bool HasInteractionMessage => !string.IsNullOrWhiteSpace(InteractionMessage);
    public string TrackActionText => SelectedProcess?.TrackActionText ?? "Track application";
    public string PauseActionText => IsPaused ? "Resume" : "Pause";
    public string PauseActionGlyph => IsPaused ? "\uE768" : "\uE769";
    public string GraphicsInsightSummary => $"{GraphicsStateTitle}. {GraphicsInsightText}";
    public string EmptyTitle => !HasSnapshot
        ? "Connecting to graphics telemetry"
        : _viewMode == GpuViewMode.Tracked
            ? "No tracked graphics workloads"
            : "No matching graphics workloads";
    public string EmptyMessage => !HasSnapshot
        ? "The process list will appear after the first complete service snapshot."
        : _viewMode == GpuViewMode.Tracked
            ? "Track a process from All graphics processes to follow the application across restarts."
            : string.IsNullOrWhiteSpace(_searchText)
                ? "No process currently has measured GPU activity or graphics memory."
                : $"No graphics process name or PID contains ‘{_searchText}’.";

    public string NameHeader => Header("Name", GpuProcessSortMode.Name);
    public string PidHeader => Header("PID", GpuProcessSortMode.Pid);
    public string GpuHeader => Header("GPU %", GpuProcessSortMode.Gpu);
    public string AverageGpuHeader => Header("1m avg", GpuProcessSortMode.AverageGpu);
    public string DedicatedHeader => Header("Dedicated MB", GpuProcessSortMode.Dedicated);
    public string DedicatedDeltaHeader => Header("1m change", GpuProcessSortMode.DedicatedDelta);
    public string SharedHeader => Header("Shared MB", GpuProcessSortMode.Shared);

    public GpuViewModel(
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
    }

    partial void OnSelectedAdapterChanged(GpuAdapterItem? value) => UpdateGraphicsInsight();

    public void Start()
    {
        if (_cts is not null)
        {
            return;
        }

        _cts = new CancellationTokenSource();
        _ = RunAsync(_cts.Token);
    }

    public void Stop()
    {
        _cts?.Cancel();
        _cts?.Dispose();
        _cts = null;
    }

    public void SetViewMode(GpuViewMode mode)
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

    public void SortBy(GpuProcessSortMode mode)
    {
        if (_sortMode == mode)
        {
            _sortDescending = !_sortDescending;
        }
        else
        {
            _sortMode = mode;
            _sortDescending = mode is not GpuProcessSortMode.Name and not GpuProcessSortMode.Pid;
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

    public async Task ToggleTrackingAsync(GpuProcessItem row)
    {
        string application = NormalizeApplication(row.Name);
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

        foreach (var process in _processOrder.Where(process =>
                     string.Equals(NormalizeApplication(process.Name), application, StringComparison.OrdinalIgnoreCase)))
        {
            process.IsTracked = tracked;
            process.SyncGpuThresholdState(ProcessGpuThreshold);
        }

        if (!tracked)
        {
            _endedTracked.RemoveAll(process =>
                string.Equals(NormalizeApplication(process.Name), application, StringComparison.OrdinalIgnoreCase));

            for (int index = _processOrder.Count - 1; index >= 0; index--)
            {
                var process = _processOrder[index];
                if (!process.IsTracked && !process.IsUsingGraphics)
                {
                    _processOrder.RemoveAt(index);
                    _processIndex.Remove(process.Identity);
                }
            }
        }

        AddChange(new GpuChangeViewModel(
            DateTimeOffset.Now,
            tracked ? GpuChangeKind.TrackingStarted : GpuChangeKind.TrackingStopped,
            tracked ? $"Tracking {row.Name}" : $"Stopped tracking {row.Name}",
            tracked
                ? "New instances will remain visible when their GPU activity falls to zero."
                : "Existing local samples remain until this page closes.",
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
                ? $"Tracking {row.Name} across restarts."
                : $"Stopped tracking {row.Name}.";
        }
        catch (Exception ex)
        {
            InteractionMessage = $"Tracking changed for this session, but could not be saved: {ex.Message}";
        }
    }

    public void DismissInteractionMessage() => InteractionMessage = string.Empty;

    private async Task RunAsync(CancellationToken cancellationToken)
    {
        while (!cancellationToken.IsCancellationRequested)
        {
            try
            {
                Post(() => StatusText = HasSnapshot ? "Reconnecting to graphics telemetry" : "Connecting to graphics telemetry");
                using var channel = AtlasChannel.Connect(_who);
                await foreach (var snapshot in channel.StreamSnapshotsAsync(0, cancellationToken).ConfigureAwait(false))
                {
                    if (cancellationToken.IsCancellationRequested)
                    {
                        break;
                    }

                    Post(() => Apply(snapshot));
                }

                if (!cancellationToken.IsCancellationRequested)
                {
                    Post(() => StatusText = "Graphics stream ended; reconnecting");
                }
            }
            catch (OperationCanceledException)
            {
                break;
            }
            catch (Exception ex)
            {
                Post(() =>
                {
                    IsUnavailable = !HasAdapter;
                    UnavailableReason = $"Graphics telemetry could not be read: {ex.Message}";
                    StatusText = "Graphics stream interrupted; reconnecting";
                });
            }

            try
            {
                await Task.Delay(TimeSpan.FromSeconds(2), cancellationToken).ConfigureAwait(false);
            }
            catch (OperationCanceledException)
            {
                break;
            }
        }
    }

    private void Apply(SnapshotReply snapshot)
    {
        if (IsPaused)
        {
            _pendingSnapshot = snapshot;
            return;
        }

        ApplyCore(snapshot);
    }

    private void ApplyCore(SnapshotReply snapshot)
    {
        long timestampMs = snapshot.System?.TsMs ?? DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
        string? selectedKey = SelectedAdapter?.AdapterKey;
        var seenAdapters = new HashSet<string>(StringComparer.Ordinal);

        foreach (var adapter in snapshot.GpuAdapters)
        {
            seenAdapters.Add(adapter.AdapterKey);
            var item = Adapters.FirstOrDefault(existing => existing.AdapterKey == adapter.AdapterKey);
            bool isNew = item is null;
            if (item is null)
            {
                item = new GpuAdapterItem(adapter.AdapterKey);
                Adapters.Add(item);
            }

            bool hadSamples = item.HasSamples;
            bool wasThrottling = item.IsThermalThrottling;
            item.Apply(adapter, timestampMs);

            if (_hasInitialSnapshot && !isNew && hadSamples)
            {
                UpdateAdapterChanges(item, wasThrottling, timestampMs);
            }
            else
            {
                item.SyncThresholdStates(
                    HighGpuThreshold,
                    MemoryPressureThreshold,
                    TemperatureMarginThreshold);
            }
        }

        for (int index = Adapters.Count - 1; index >= 0; index--)
        {
            if (!seenAdapters.Contains(Adapters[index].AdapterKey))
            {
                Adapters.RemoveAt(index);
            }
        }

        SelectedAdapter = Adapters.FirstOrDefault(adapter => adapter.AdapterKey == selectedKey)
            ?? Adapters.FirstOrDefault(adapter => adapter.ActiveDisplay)
            ?? Adapters.FirstOrDefault();

        bool processStructureChanged = ApplyProcesses(snapshot.Processes, timestampMs);

        HasSnapshot = true;
        IsUnavailable = Adapters.Count == 0;
        UnavailableReason = IsUnavailable
            ? (string.IsNullOrWhiteSpace(snapshot.GpuUnavailableReason)
                ? "Windows did not expose graphics counters for this session."
                : snapshot.GpuUnavailableReason)
            : string.Empty;
        StatusText = IsUnavailable
            ? "No measured graphics data"
            : $"Connected · {Adapters.Count} adapter{(Adapters.Count == 1 ? string.Empty : "s")} measured live";

        if (!_hasInitialSnapshot)
        {
            SortCurrentSnapshot();
            RaiseHeaderChanges();
            _hasInitialSnapshot = true;
            processStructureChanged = true;
        }

        if (processStructureChanged)
        {
            RefreshVisibleProcesses();
        }
        else
        {
            UpdateVisibleCount();
        }
        UpdateGraphicsInsight();
        OnPropertyChanged(nameof(HasAdapter));
        OnPropertyChanged(nameof(EmptyTitle));
        OnPropertyChanged(nameof(EmptyMessage));
    }

    private bool ApplyProcesses(IEnumerable<ProcessRow> snapshotRows, long timestampMs)
    {
        var snapshotIdentities = new HashSet<ProcessIdentity>();
        bool structureChanged = false;

        foreach (var source in snapshotRows)
        {
            var identity = new ProcessIdentity(source.Pid, source.CreateTime100Ns);
            snapshotIdentities.Add(identity);
            bool tracked = _trackedApplications.Contains(NormalizeApplication(source.ImageName));
            bool measured = GpuProcessItem.HasMeasuredGraphics(source);
            bool exists = _processIndex.TryGetValue(identity, out var process);

            if (!exists && !tracked && !measured)
            {
                continue;
            }

            if (!exists)
            {
                process = new GpuProcessItem(source.Pid, source.CreateTime100Ns)
                {
                    IsTracked = tracked,
                };
                _processIndex.Add(identity, process);
                _processOrder.Add(process);
                structureChanged = true;
            }

            bool hadSamples = process!.HasSamples;
            bool wasUsingGraphics = process.IsUsingGraphics;
            process.IsTracked = tracked;
            process.Update(source, timestampMs);

            if (_hasInitialSnapshot && hadSamples && process.IsTracked)
            {
                if (!wasUsingGraphics && process.IsUsingGraphics)
                {
                    AddChange(new GpuChangeViewModel(
                        DateTimeOffset.FromUnixTimeMilliseconds(timestampMs),
                        GpuChangeKind.WorkloadStarted,
                        $"{process.Name} started using graphics resources",
                        $"PID {process.Pid}; GPU {process.GpuText}%",
                        true));
                }
                else if (wasUsingGraphics && !process.IsUsingGraphics)
                {
                    AddChange(new GpuChangeViewModel(
                        DateTimeOffset.FromUnixTimeMilliseconds(timestampMs),
                        GpuChangeKind.WorkloadStopped,
                        $"{process.Name} graphics activity stopped",
                        $"PID {process.Pid}; the process is still running",
                        true));
                }

                if (!process.IsGpuElevatedState && process.GpuPercent >= ProcessGpuThreshold)
                {
                    process.IsGpuElevatedState = true;
                    AddChange(new GpuChangeViewModel(
                        DateTimeOffset.FromUnixTimeMilliseconds(timestampMs),
                        GpuChangeKind.ProcessGpuRaised,
                        $"{process.Name} GPU use rose",
                        $"{process.GpuText}% now; {process.AverageGpuText}% one-minute average",
                        true));
                }
                else if (process.IsGpuElevatedState && process.GpuPercent <= ProcessGpuSettleThreshold)
                {
                    process.IsGpuElevatedState = false;
                    AddChange(new GpuChangeViewModel(
                        DateTimeOffset.FromUnixTimeMilliseconds(timestampMs),
                        GpuChangeKind.ProcessGpuSettled,
                        $"{process.Name} GPU use settled",
                        $"{process.GpuText}% now after exceeding {ProcessGpuThreshold:F0}%",
                        true));
                }
            }
            else
            {
                process.SyncGpuThresholdState(ProcessGpuThreshold);
            }

            if (wasUsingGraphics != process.IsUsingGraphics)
            {
                structureChanged = true;
            }
        }

        for (int index = _processOrder.Count - 1; index >= 0; index--)
        {
            var process = _processOrder[index];
            if (!snapshotIdentities.Contains(process.Identity))
            {
                bool wasTracked = process.IsTracked;
                bool wasUsing = process.IsUsingGraphics;
                process.MarkEnded(timestampMs);
                _processOrder.RemoveAt(index);
                _processIndex.Remove(process.Identity);
                structureChanged = true;

                if (wasTracked)
                {
                    _endedTracked.Insert(0, process);
                    if (_endedTracked.Count > 50)
                    {
                        _endedTracked.RemoveAt(_endedTracked.Count - 1);
                    }

                    if (_hasInitialSnapshot && wasUsing)
                    {
                        AddChange(new GpuChangeViewModel(
                            DateTimeOffset.FromUnixTimeMilliseconds(timestampMs),
                            GpuChangeKind.WorkloadStopped,
                            $"{process.Name} ended",
                            $"PID {process.Pid}; last GPU {process.GpuText}%",
                            true));
                    }
                }

                if (ReferenceEquals(SelectedProcess, process))
                {
                    OnPropertyChanged(nameof(CanInspectSelection));
                }
            }
            else if (!process.IsTracked && !process.IsUsingGraphics)
            {
                _processOrder.RemoveAt(index);
                _processIndex.Remove(process.Identity);
                structureChanged = true;
            }
        }

        return structureChanged;
    }

    private void UpdateAdapterChanges(
        GpuAdapterItem adapter,
        bool wasThrottling,
        long timestampMs)
    {
        var timestamp = DateTimeOffset.FromUnixTimeMilliseconds(timestampMs);
        if (!adapter.IsHighGpuState && adapter.UtilizationPercent >= HighGpuThreshold)
        {
            adapter.IsHighGpuState = true;
            AddChange(new GpuChangeViewModel(
                timestamp,
                GpuChangeKind.AdapterGpuRaised,
                $"{adapter.Name} load is high",
                $"{adapter.UtilizationText} now; {adapter.AverageUtilizationText} one-minute average",
                false));
        }
        else if (adapter.IsHighGpuState && adapter.UtilizationPercent <= HighGpuSettleThreshold)
        {
            adapter.IsHighGpuState = false;
            AddChange(new GpuChangeViewModel(
                timestamp,
                GpuChangeKind.AdapterGpuSettled,
                $"{adapter.Name} load settled",
                $"{adapter.UtilizationText} now after exceeding {HighGpuThreshold:F0}%",
                false));
        }

        if (!adapter.IsMemoryPressureState && adapter.DedicatedPercent >= MemoryPressureThreshold)
        {
            adapter.IsMemoryPressureState = true;
            AddChange(new GpuChangeViewModel(
                timestamp,
                GpuChangeKind.MemoryPressure,
                $"{adapter.Name} dedicated memory is near budget",
                $"{adapter.DedicatedText} ({adapter.DedicatedPercent:F0}%)",
                false));
        }
        else if (adapter.IsMemoryPressureState && adapter.DedicatedPercent <= MemorySettleThreshold)
        {
            adapter.IsMemoryPressureState = false;
            AddChange(new GpuChangeViewModel(
                timestamp,
                GpuChangeKind.MemorySettled,
                $"{adapter.Name} dedicated memory pressure cleared",
                $"{adapter.DedicatedText} ({adapter.DedicatedPercent:F0}%)",
                false));
        }

        double temperatureMargin = adapter.HasTemperature && adapter.HasTemperatureWarning
            ? adapter.TemperatureWarningC - adapter.TemperatureC
            : double.PositiveInfinity;
        if (!adapter.IsNearWarningState && temperatureMargin <= TemperatureMarginThreshold)
        {
            adapter.IsNearWarningState = true;
            AddChange(new GpuChangeViewModel(
                timestamp,
                GpuChangeKind.TemperatureNearLimit,
                $"{adapter.Name} temperature is close to warning",
                adapter.TemperatureMarginText,
                false));
        }
        else if (adapter.IsNearWarningState && temperatureMargin >= TemperatureSettleMargin)
        {
            adapter.IsNearWarningState = false;
            AddChange(new GpuChangeViewModel(
                timestamp,
                GpuChangeKind.TemperatureSettled,
                $"{adapter.Name} temperature moved away from warning",
                adapter.TemperatureMarginText,
                false));
        }

        if (!wasThrottling && adapter.IsThermalThrottling)
        {
            AddChange(new GpuChangeViewModel(
                timestamp,
                GpuChangeKind.ThrottleStarted,
                $"{adapter.Name} reports thermal throttling",
                adapter.ThrottleText,
                false));
        }
        else if (wasThrottling && !adapter.IsThermalThrottling)
        {
            AddChange(new GpuChangeViewModel(
                timestamp,
                GpuChangeKind.ThrottleStopped,
                $"{adapter.Name} thermal throttling cleared",
                adapter.TemperatureText,
                false));
        }
    }

    private void UpdateGraphicsInsight()
    {
        var adapter = SelectedAdapter;
        if (adapter is null)
        {
            GraphicsStateTitle = HasSnapshot ? "No graphics adapter is available" : "Measuring graphics behavior";
            GraphicsInsightText = HasSnapshot
                ? UnavailableReason
                : "A factual interpretation will appear after live samples arrive.";
            TopProcessText = "No measured process";
            return;
        }

        var topProcess = _processOrder
            .Where(process => process.IsUsingGraphics)
            .OrderByDescending(process => process.GpuPercent)
            .ThenBy(process => process.Name, StringComparer.OrdinalIgnoreCase)
            .FirstOrDefault();
        TopProcessText = topProcess is null
            ? "No measured process"
            : $"{topProcess.Name} · {topProcess.GpuText}%";

        GraphicsStateTitle = adapter.IsThermalThrottling
            ? "Thermal throttling is active"
            : adapter.IsNearTemperatureWarning
                ? "Temperature is close to the warning limit"
                : adapter.DedicatedPercent >= MemoryPressureThreshold
                    ? "Dedicated memory is near its budget"
                    : adapter.UtilizationPercent >= HighGpuThreshold
                        ? "Graphics load is high"
                        : adapter.UtilizationPercent >= 30
                            ? "Graphics workload is active"
                            : "Graphics load is light";

        string duration = adapter.SampleSpanSeconds >= 55
            ? "Over the last minute"
            : $"Across {Math.Max(1, adapter.SampleSpanSeconds):F0} seconds";
        string memory = adapter.DedicatedBudgetMb > 0
            ? $"dedicated memory is {adapter.DedicatedPercent:F0}% of budget"
            : $"{adapter.DedicatedUsedMb:F0} MB of dedicated memory is measured; the budget is unavailable";
        string temperature = adapter.HasTemperature
            ? adapter.HasTemperatureWarning
                ? $"temperature is {adapter.TemperatureC:F1} °C, {Math.Max(0, adapter.TemperatureWarningC - adapter.TemperatureC):F1} °C below warning"
                : $"temperature is {adapter.TemperatureC:F1} °C; no warning limit is available"
            : "temperature is unavailable";
        string throttle = adapter.IsThermalThrottling
            ? "thermal throttling is reported"
            : "no explicit thermal throttle is reported";

        GraphicsInsightText =
            $"{duration}, GPU averaged {adapter.AverageUtilizationOneMinute:F1}% and peaked at {adapter.PeakUtilizationOneMinute:F1}%; " +
            $"{memory}; {temperature}; {throttle}.";
    }

    private void AddChange(GpuChangeViewModel change)
    {
        Changes.Insert(0, change);
        while (Changes.Count > ChangeLimit)
        {
            Changes.RemoveAt(Changes.Count - 1);
        }

        HasChanges = Changes.Count > 0;
        if (_viewMode == GpuViewMode.Changes)
        {
            UpdateVisibleCount();
        }
    }

    private void SortCurrentSnapshot()
    {
        IOrderedEnumerable<GpuProcessItem> ordered = (_sortMode, _sortDescending) switch
        {
            (GpuProcessSortMode.Name, false) => _processOrder.OrderBy(process => process.Name, StringComparer.OrdinalIgnoreCase),
            (GpuProcessSortMode.Name, true) => _processOrder.OrderByDescending(process => process.Name, StringComparer.OrdinalIgnoreCase),
            (GpuProcessSortMode.Pid, false) => _processOrder.OrderBy(process => process.Pid),
            (GpuProcessSortMode.Pid, true) => _processOrder.OrderByDescending(process => process.Pid),
            (GpuProcessSortMode.Gpu, false) => _processOrder.OrderBy(process => process.GpuPercent),
            (GpuProcessSortMode.Gpu, true) => _processOrder.OrderByDescending(process => process.GpuPercent),
            (GpuProcessSortMode.AverageGpu, false) => _processOrder.OrderBy(process => process.AverageGpuOneMinute),
            (GpuProcessSortMode.AverageGpu, true) => _processOrder.OrderByDescending(process => process.AverageGpuOneMinute),
            (GpuProcessSortMode.Dedicated, false) => _processOrder.OrderBy(process => process.DedicatedMb),
            (GpuProcessSortMode.Dedicated, true) => _processOrder.OrderByDescending(process => process.DedicatedMb),
            (GpuProcessSortMode.DedicatedDelta, false) => _processOrder.OrderBy(process => process.DedicatedDeltaOneMinuteMb),
            (GpuProcessSortMode.DedicatedDelta, true) => _processOrder.OrderByDescending(process => process.DedicatedDeltaOneMinuteMb),
            (GpuProcessSortMode.Shared, false) => _processOrder.OrderBy(process => process.SharedMb),
            _ => _processOrder.OrderByDescending(process => process.SharedMb),
        };

        var orderedRows = ordered
            .ThenBy(process => process.Name, StringComparer.OrdinalIgnoreCase)
            .ThenBy(process => process.Pid)
            .ToArray();
        _processOrder.Clear();
        _processOrder.AddRange(orderedRows);
    }

    private void RefreshVisibleProcesses()
    {
        var selected = SelectedProcess;
        IEnumerable<GpuProcessItem> source = _viewMode switch
        {
            GpuViewMode.Tracked => _processOrder.Where(process => process.IsTracked).Concat(_endedTracked),
            GpuViewMode.Changes => Array.Empty<GpuProcessItem>(),
            _ => _processOrder.Where(process => process.IsUsingGraphics),
        };

        if (!string.IsNullOrWhiteSpace(_searchText))
        {
            source = source.Where(process =>
                process.Name.Contains(_searchText, StringComparison.OrdinalIgnoreCase)
                || process.Pid.ToString(CultureInfo.InvariantCulture).Contains(_searchText, StringComparison.OrdinalIgnoreCase)
                || process.AppGroup.Contains(_searchText, StringComparison.OrdinalIgnoreCase));
        }

        ReconcileVisibleProcesses(source.ToArray());

        HasVisibleProcesses = Processes.Count > 0;
        if (selected is not null && Processes.Contains(selected))
        {
            SelectedProcess = selected;
        }
        else
        {
            SelectedProcess = null;
        }

        UpdateVisibleCount();
        OnPropertyChanged(nameof(EmptyTitle));
        OnPropertyChanged(nameof(EmptyMessage));
    }

    private void ReconcileVisibleProcesses(IReadOnlyList<GpuProcessItem> target)
    {
        for (int targetIndex = 0; targetIndex < target.Count; targetIndex++)
        {
            var expected = target[targetIndex];
            if (targetIndex < Processes.Count && ReferenceEquals(Processes[targetIndex], expected))
            {
                continue;
            }

            int existingIndex = Processes.IndexOf(expected);
            if (existingIndex >= 0)
            {
                Processes.Move(existingIndex, targetIndex);
            }
            else
            {
                Processes.Insert(targetIndex, expected);
            }
        }

        while (Processes.Count > target.Count)
        {
            Processes.RemoveAt(Processes.Count - 1);
        }
    }

    private void UpdateVisibleCount()
    {
        int measuredCount = _processOrder.Count(process => process.IsUsingGraphics);
        VisibleCountText = _viewMode switch
        {
            GpuViewMode.Tracked => $"{Processes.Count} tracked instance{(Processes.Count == 1 ? string.Empty : "s")}",
            GpuViewMode.Changes => $"{Changes.Count} measured change{(Changes.Count == 1 ? string.Empty : "s")}",
            _ => $"{Processes.Count} shown of {measuredCount} graphics processes",
        };
    }

    private string Header(string label, GpuProcessSortMode mode) =>
        _sortMode == mode ? $"{label} {(_sortDescending ? "↓" : "↑")}" : label;

    private void RaiseHeaderChanges()
    {
        OnPropertyChanged(nameof(NameHeader));
        OnPropertyChanged(nameof(PidHeader));
        OnPropertyChanged(nameof(GpuHeader));
        OnPropertyChanged(nameof(AverageGpuHeader));
        OnPropertyChanged(nameof(DedicatedHeader));
        OnPropertyChanged(nameof(DedicatedDeltaHeader));
        OnPropertyChanged(nameof(SharedHeader));
        OnPropertyChanged(nameof(SortMode));
        OnPropertyChanged(nameof(SortDescending));
    }

    private static string NormalizeApplication(string imageName) => imageName.Trim();

    private void Post(Action action) => _dispatcher.TryEnqueue(() => action());
}

public readonly record struct GpuAdapterLiveSample(
    long TimestampMs,
    double UtilizationPercent,
    double DedicatedUsedMb,
    double SharedUsedMb,
    double? TemperatureC,
    double? PowerW);

public sealed partial class GpuAdapterItem : ObservableObject
{
    private const double TemperatureMarginThresholdC = 5;
    private static readonly CultureInfo Inv = CultureInfo.InvariantCulture;
    private readonly Queue<GpuAdapterLiveSample> _samples = new();

    internal bool IsHighGpuState { get; set; }
    internal bool IsMemoryPressureState { get; set; }
    internal bool IsNearWarningState { get; set; }

    public string AdapterKey { get; }
    public ObservableCollection<GpuEngineItem> Engines { get; } = new();
    public ObservableCollection<GpuTemperatureItem> AdditionalTemperatures { get; } = new();
    public ObservableCollection<GpuAvailabilityItem> Availability { get; } = new();

    [ObservableProperty] private string _name = "GPU";
    [ObservableProperty] private string _driverVersion = "Driver version unavailable";
    [ObservableProperty] private string _driverDate = "Driver date unavailable";
    [ObservableProperty] private string _pciLocation = "PCI location unavailable";
    [ObservableProperty] private string _adapterIdentity = string.Empty;
    [ObservableProperty] [NotifyPropertyChangedFor(nameof(DisplayRole))] private bool _activeDisplay;
    [ObservableProperty] [NotifyPropertyChangedFor(nameof(UtilizationText))] private double _utilizationPercent;
    [ObservableProperty] [NotifyPropertyChangedFor(nameof(DedicatedText))] [NotifyPropertyChangedFor(nameof(DedicatedPercent))] private double _dedicatedUsedMb;
    [ObservableProperty] [NotifyPropertyChangedFor(nameof(DedicatedText))] [NotifyPropertyChangedFor(nameof(DedicatedPercent))] private double _dedicatedBudgetMb;
    [ObservableProperty] [NotifyPropertyChangedFor(nameof(SharedText))] private double _sharedUsedMb;
    [ObservableProperty] [NotifyPropertyChangedFor(nameof(SharedText))] private double _sharedBudgetMb;
    [ObservableProperty] [NotifyPropertyChangedFor(nameof(TemperatureText))] [NotifyPropertyChangedFor(nameof(TemperatureMarginText))] [NotifyPropertyChangedFor(nameof(IsNearTemperatureWarning))] private bool _hasTemperature;
    [ObservableProperty] [NotifyPropertyChangedFor(nameof(TemperatureText))] [NotifyPropertyChangedFor(nameof(TemperatureMarginText))] [NotifyPropertyChangedFor(nameof(IsNearTemperatureWarning))] private double _temperatureC;
    [ObservableProperty] [NotifyPropertyChangedFor(nameof(TemperatureMarginText))] [NotifyPropertyChangedFor(nameof(IsNearTemperatureWarning))] private bool _hasTemperatureWarning;
    [ObservableProperty] [NotifyPropertyChangedFor(nameof(TemperatureMarginText))] [NotifyPropertyChangedFor(nameof(IsNearTemperatureWarning))] private double _temperatureWarningC;
    [ObservableProperty] private bool _hasTemperatureMax;
    [ObservableProperty] private double _temperatureMaxC;
    [ObservableProperty] [NotifyPropertyChangedFor(nameof(PowerWattsText))] private bool _hasPowerWatts;
    [ObservableProperty] [NotifyPropertyChangedFor(nameof(PowerWattsText))] private double _powerWatts;
    [ObservableProperty] private string _powerPercentText = "Unavailable";
    [ObservableProperty] private string _coreClockText = "Unavailable";
    [ObservableProperty] private string _memoryClockText = "Unavailable";
    [ObservableProperty] private string _fanRpmText = "Unavailable";
    [ObservableProperty] private string _fanPercentText = "Unavailable";
    [ObservableProperty] private string _temperatureLimitsText = "Limits unavailable";
    [ObservableProperty] private string _throttleText = "Unavailable";
    [ObservableProperty] private string _sensorStatus = string.Empty;
    [ObservableProperty] private bool _isThermalThrottling;
    [ObservableProperty] [NotifyPropertyChangedFor(nameof(AverageUtilizationText))] private double _averageUtilizationOneMinute;
    [ObservableProperty] [NotifyPropertyChangedFor(nameof(PeakUtilizationText))] private double _peakUtilizationOneMinute;
    [ObservableProperty] private double _dedicatedDeltaOneMinuteMb;
    [ObservableProperty] private double _temperatureDeltaOneMinuteC;
    [ObservableProperty] private double _sampleSpanSeconds;

    public event Action<GpuAdapterItem>? SamplesChanged;

    public bool HasSamples => _samples.Count > 0;
    public string UtilizationText => UtilizationPercent.ToString("F1", Inv) + " %";
    public string AverageUtilizationText => AverageUtilizationOneMinute.ToString("F1", Inv) + " %";
    public string PeakUtilizationText => PeakUtilizationOneMinute.ToString("F1", Inv) + " %";
    public double DedicatedPercent => DedicatedBudgetMb > 0 ? DedicatedUsedMb / DedicatedBudgetMb * 100 : 0;
    public string DedicatedText => DedicatedBudgetMb > 0
        ? $"{DedicatedUsedMb:F0} / {DedicatedBudgetMb:F0} MB"
        : $"{DedicatedUsedMb:F0} MB measured";
    public string SharedText => SharedBudgetMb > 0
        ? $"{SharedUsedMb:F0} / {SharedBudgetMb:F0} MB"
        : $"{SharedUsedMb:F0} MB measured";
    public string TemperatureText => HasTemperature ? $"{TemperatureC:F1} °C" : "Unavailable";
    public string TemperatureMarginText => HasTemperature && HasTemperatureWarning
        ? $"{TemperatureC:F1} °C; {Math.Max(0, TemperatureWarningC - TemperatureC):F1} °C below warning"
        : TemperatureText;
    public bool IsNearTemperatureWarning =>
        HasTemperature && HasTemperatureWarning && TemperatureWarningC - TemperatureC <= TemperatureMarginThresholdC;
    public string PowerWattsText => HasPowerWatts ? $"{PowerWatts:F1} W" : "Unavailable";
    public string DisplayRole => ActiveDisplay ? "Active display adapter" : "Available adapter";

    public GpuAdapterItem(string key) => AdapterKey = key;

    public IReadOnlyList<GpuAdapterLiveSample> GetSamples() => _samples.ToArray();

    internal void SyncThresholdStates(
        double highGpuThreshold,
        double memoryPressureThreshold,
        double temperatureMarginThreshold)
    {
        IsHighGpuState = UtilizationPercent >= highGpuThreshold;
        IsMemoryPressureState = DedicatedPercent >= memoryPressureThreshold;
        IsNearWarningState = HasTemperature
            && HasTemperatureWarning
            && TemperatureWarningC - TemperatureC <= temperatureMarginThreshold;
    }

    public void Apply(GpuAdapterTelemetry source, long timestampMs)
    {
        Name = source.Name;
        DriverVersion = string.IsNullOrWhiteSpace(source.DriverVersion)
            ? "Driver version unavailable"
            : $"Driver {source.DriverVersion}";
        DriverDate = string.IsNullOrWhiteSpace(source.DriverDate)
            ? "Driver date unavailable"
            : $"Driver date {source.DriverDate}";
        ActiveDisplay = source.ActiveDisplay;
        PciLocation = source.PciIdentityAvailable
            ? $"PCI {source.PciDomain:X4}:{source.PciBus:X2}:{source.PciDevice:X2}.{source.PciFunction}"
            : "PCI location unavailable";
        AdapterIdentity = $"VEN_{source.VendorId:X4} · DEV_{source.DeviceId:X4} · physical {source.PhysicalAdapterIndex}";
        UtilizationPercent = Sanitize(source.UtilizationPermille / 10.0);
        DedicatedUsedMb = BytesToMb(source.DedicatedUsed);
        DedicatedBudgetMb = BytesToMb(source.DedicatedBudget);
        SharedUsedMb = BytesToMb(source.SharedUsed);
        SharedBudgetMb = BytesToMb(source.SharedBudget);
        HasTemperature = source.HasTemperatureC;
        TemperatureC = source.HasTemperatureC ? Sanitize(source.TemperatureC) : 0;
        HasTemperatureWarning = source.HasTemperatureWarningC;
        TemperatureWarningC = source.HasTemperatureWarningC ? Sanitize(source.TemperatureWarningC) : 0;
        HasTemperatureMax = source.HasTemperatureMaxC;
        TemperatureMaxC = source.HasTemperatureMaxC ? Sanitize(source.TemperatureMaxC) : 0;
        HasPowerWatts = source.HasPowerW;
        PowerWatts = source.HasPowerW ? Sanitize(source.PowerW) : 0;
        PowerPercentText = Reading(source.HasPowerPercent, source.HasPowerPercent ? $"{source.PowerPercent:F1} %" : null, source, GpuSensorKind.GpuSensorPowerPercent);
        CoreClockText = Reading(source.HasCoreClockMhz, source.HasCoreClockMhz ? $"{source.CoreClockMhz} MHz" : null, source, GpuSensorKind.GpuSensorCoreClock);
        MemoryClockText = Reading(source.HasMemoryClockMhz, source.HasMemoryClockMhz ? $"{source.MemoryClockMhz} MHz" : null, source, GpuSensorKind.GpuSensorMemoryClock);
        FanRpmText = Reading(source.HasFanRpm, source.HasFanRpm ? $"{source.FanRpm} RPM" : null, source, GpuSensorKind.GpuSensorFanRpm);
        FanPercentText = Reading(source.HasFanPercent, source.HasFanPercent ? $"{source.FanPercent:F1} %" : null, source, GpuSensorKind.GpuSensorFanPercent);
        TemperatureLimitsText = TemperatureLimits(source);
        IsThermalThrottling = source.HasThermalThrottling && source.ThermalThrottling;
        ThrottleText = ThrottleState(source);
        SensorStatus = ProviderStatus(source);

        UpdateEngines(source.Engines);

        AdditionalTemperatures.Clear();
        foreach (var temperature in source.Temperatures.Where(item => item.Kind != GpuTemperatureKind.GpuTemperatureCore))
        {
            AdditionalTemperatures.Add(new GpuTemperatureItem(temperature));
        }

        Availability.Clear();
        foreach (var availability in source.SensorAvailability
                     .Where(item => !item.Available)
                     .OrderBy(item => item.Source)
                     .ThenBy(item => item.Kind))
        {
            Availability.Add(new GpuAvailabilityItem(availability));
        }

        long safeTimestamp = timestampMs > 0 ? timestampMs : DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
        _samples.Enqueue(new GpuAdapterLiveSample(
            safeTimestamp,
            UtilizationPercent,
            DedicatedUsedMb,
            SharedUsedMb,
            HasTemperature ? TemperatureC : null,
            HasPowerWatts ? PowerWatts : null));
        long cutoff = safeTimestamp - (long)TimeSpan.FromMinutes(10).TotalMilliseconds;
        while (_samples.Count > 1 && (_samples.Peek().TimestampMs < cutoff || _samples.Count > 600))
        {
            _samples.Dequeue();
        }

        UpdateOneMinuteSummary(safeTimestamp);
        SamplesChanged?.Invoke(this);
    }

    private void UpdateEngines(IEnumerable<GpuEngineTelemetry> engines)
    {
        var seen = new HashSet<GpuEngineClass>();
        foreach (var source in engines.OrderBy(engine => engine.EngineClass))
        {
            seen.Add(source.EngineClass);
            var item = Engines.FirstOrDefault(engine => engine.EngineClass == source.EngineClass);
            if (item is null)
            {
                item = new GpuEngineItem(source.EngineClass);
                Engines.Add(item);
            }
            item.Update(source);
        }

        for (int index = Engines.Count - 1; index >= 0; index--)
        {
            if (!seen.Contains(Engines[index].EngineClass))
            {
                Engines.RemoveAt(index);
            }
        }
    }

    private void UpdateOneMinuteSummary(long timestampMs)
    {
        long cutoff = timestampMs - (long)TimeSpan.FromMinutes(1).TotalMilliseconds;
        var recent = _samples.Where(sample => sample.TimestampMs >= cutoff).ToArray();
        if (recent.Length == 0)
        {
            return;
        }

        AverageUtilizationOneMinute = recent.Average(sample => sample.UtilizationPercent);
        PeakUtilizationOneMinute = recent.Max(sample => sample.UtilizationPercent);
        DedicatedDeltaOneMinuteMb = DedicatedUsedMb - recent[0].DedicatedUsedMb;
        var temperatures = recent.Where(sample => sample.TemperatureC.HasValue).ToArray();
        TemperatureDeltaOneMinuteC = temperatures.Length > 0 && HasTemperature
            ? TemperatureC - temperatures[0].TemperatureC!.Value
            : 0;
        SampleSpanSeconds = Math.Max(0, (timestampMs - recent[0].TimestampMs) / 1000.0);
    }

    private static string Reading(bool present, string? value, GpuAdapterTelemetry adapter, GpuSensorKind kind)
    {
        var available = adapter.SensorAvailability
            .Where(item => item.Kind == kind && item.Available)
            .OrderByDescending(item => item.Source == GpuTelemetrySource.GpuSourceNvidiaNvml)
            .FirstOrDefault();
        if (present && value is not null)
        {
            return available is null ? value : $"{value} · {SourceName(available.Source)}";
        }

        var unavailable = adapter.SensorAvailability
            .Where(item => item.Kind == kind && !item.Available)
            .OrderByDescending(item => item.Source == GpuTelemetrySource.GpuSourceNvidiaNvml)
            .FirstOrDefault();
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
        {
            return Reading(false, null, adapter, GpuSensorKind.GpuSensorThrottleReasons);
        }
        if (!adapter.ThermalThrottling)
        {
            return "No explicit thermal throttle · NVIDIA NVML";
        }
        return adapter.ThrottleReasons.Count == 0
            ? "Thermal throttle reported · NVIDIA NVML"
            : $"{string.Join(", ", adapter.ThrottleReasons.Select(ThrottleName))} · NVIDIA NVML";
    }

    private static string ProviderStatus(GpuAdapterTelemetry adapter)
    {
        bool nvmlActive = adapter.SensorAvailability.Any(item =>
            item.Source == GpuTelemetrySource.GpuSourceNvidiaNvml && item.Available);
        var nvmlFailure = adapter.SensorAvailability.FirstOrDefault(item =>
            item.Source == GpuTelemetrySource.GpuSourceNvidiaNvml
            && !item.Available
            && item.Reason != GpuAvailabilityReason.GpuAvailabilityUnsupportedMetric);
        if (nvmlActive)
        {
            return "NVIDIA NVML is active. Unsupported fields retain their current Windows WDDM reading.";
        }
        if (nvmlFailure is not null)
        {
            return $"Windows WDDM fallback · {ReasonCode(nvmlFailure.Reason)} · {nvmlFailure.Detail}";
        }
        return string.IsNullOrWhiteSpace(adapter.SensorSource)
            ? adapter.SensorUnavailableReason
            : adapter.SensorSource;
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

    private static double BytesToMb(ulong bytes) => bytes / (1024.0 * 1024.0);

    private static double Sanitize(double value) => double.IsFinite(value) ? Math.Max(0, value) : 0;
}

public sealed partial class GpuEngineItem : ObservableObject
{
    private static readonly CultureInfo Inv = CultureInfo.InvariantCulture;
    public GpuEngineClass EngineClass { get; }
    public string Name { get; }

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(PercentText))]
    private double _percent;

    public string PercentText => Percent.ToString("F1", Inv) + " %";

    public GpuEngineItem(GpuEngineClass engineClass)
    {
        EngineClass = engineClass;
        Name = engineClass switch
        {
            GpuEngineClass.GpuEngine3D => "3D",
            GpuEngineClass.GpuEngineCompute => "Compute",
            GpuEngineClass.GpuEngineCopy => "Copy",
            GpuEngineClass.GpuEngineVideoEncode => "Video encode",
            GpuEngineClass.GpuEngineVideoDecode => "Video decode",
            _ => "Other",
        };
    }

    public void Update(GpuEngineTelemetry source) => Percent = source.UtilizationPermille / 10.0;
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
        Detail = $"{GpuAdapterItem.SourceName(availability.Source)} · {GpuAdapterItem.ReasonCode(availability.Reason)}"
            + (string.IsNullOrWhiteSpace(availability.Detail) ? string.Empty : $" · {availability.Detail}");
    }
}

public readonly record struct GpuProcessLiveSample(
    long TimestampMs,
    double GpuPercent,
    double DedicatedMb,
    double SharedMb);

public sealed partial class GpuProcessItem : ObservableObject
{
    private static readonly CultureInfo Inv = CultureInfo.InvariantCulture;
    private readonly Queue<GpuProcessLiveSample> _samples = new();

    internal bool IsGpuElevatedState { get; set; }

    public ProcessIdentity Identity { get; }
    public uint Pid => Identity.Pid;
    public long CreateTime100ns => Identity.CreateTime100ns;

    [ObservableProperty] [NotifyPropertyChangedFor(nameof(AccessibleSummary))] private string _name = string.Empty;
    [ObservableProperty] private string _appGroup = string.Empty;
    [ObservableProperty] [NotifyPropertyChangedFor(nameof(GpuText))] [NotifyPropertyChangedFor(nameof(AccessibleSummary))] private double _gpuPercent;
    [ObservableProperty] [NotifyPropertyChangedFor(nameof(DedicatedText))] [NotifyPropertyChangedFor(nameof(AccessibleSummary))] private double _dedicatedMb;
    [ObservableProperty] [NotifyPropertyChangedFor(nameof(SharedText))] [NotifyPropertyChangedFor(nameof(AccessibleSummary))] private double _sharedMb;
    [ObservableProperty] [NotifyPropertyChangedFor(nameof(IsUsingText))] [NotifyPropertyChangedFor(nameof(StatusText))] [NotifyPropertyChangedFor(nameof(AccessibleSummary))] private bool _isUsingGraphics;
    [ObservableProperty] [NotifyPropertyChangedFor(nameof(TrackActionText))] [NotifyPropertyChangedFor(nameof(TrackGlyph))] [NotifyPropertyChangedFor(nameof(StatusText))] [NotifyPropertyChangedFor(nameof(AccessibleSummary))] private bool _isTracked;
    [ObservableProperty] [NotifyPropertyChangedFor(nameof(StatusText))] [NotifyPropertyChangedFor(nameof(AccessibleSummary))] private bool _isRunning = true;
    [ObservableProperty] [NotifyPropertyChangedFor(nameof(AverageGpuText))] private double _averageGpuOneMinute;
    [ObservableProperty] [NotifyPropertyChangedFor(nameof(PeakGpuText))] private double _peakGpuOneMinute;
    [ObservableProperty] [NotifyPropertyChangedFor(nameof(DedicatedDeltaText))] private double _dedicatedDeltaOneMinuteMb;
    [ObservableProperty] private long _lastSeenMs;

    public event Action<GpuProcessItem>? SamplesChanged;

    public bool HasSamples => _samples.Count > 0;
    public string PidText => Pid.ToString(Inv);
    public string GpuText => GpuPercent.ToString("F1", Inv);
    public string AverageGpuText => AverageGpuOneMinute.ToString("F1", Inv);
    public string PeakGpuText => PeakGpuOneMinute.ToString("F1", Inv);
    public string DedicatedText => DedicatedMb.ToString("F0", Inv);
    public string SharedText => SharedMb.ToString("F0", Inv);
    public string DedicatedDeltaText => FormatSigned(DedicatedDeltaOneMinuteMb);
    public string TrackActionText => IsTracked ? "Stop tracking application" : "Track application";
    public string TrackGlyph => IsTracked ? "\uE735" : "\uE734";
    public string IsUsingText => IsUsingGraphics ? "Using graphics resources" : "No current graphics activity";
    public string StatusText => IsRunning
        ? IsTracked
            ? IsUsingGraphics ? "Tracked, active" : "Tracked, idle"
            : "Active"
        : "Ended";
    public string AccessibleSummary =>
        $"{Name}, PID {Pid}, GPU {GpuText} percent, one-minute average {AverageGpuText} percent, " +
        $"dedicated memory {DedicatedText} megabytes, shared memory {SharedText} megabytes, {StatusText}.";

    public GpuProcessItem(uint pid, long createTime100ns) =>
        Identity = new ProcessIdentity(pid, createTime100ns);

    public override string ToString() => AccessibleSummary;

    public IReadOnlyList<GpuProcessLiveSample> GetSamples() => _samples.ToArray();

    internal void SyncGpuThresholdState(double threshold) =>
        IsGpuElevatedState = GpuPercent >= threshold;

    public void Update(ProcessRow source, long timestampMs)
    {
        Name = source.ImageName;
        AppGroup = source.AppGroup;
        GpuPercent = source.GpuPermille / 10.0;
        DedicatedMb = source.GpuDedicatedBytes / (1024.0 * 1024.0);
        SharedMb = source.GpuSharedBytes / (1024.0 * 1024.0);
        IsUsingGraphics = HasMeasuredGraphics(source);
        IsRunning = true;
        LastSeenMs = timestampMs > 0 ? timestampMs : DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();

        _samples.Enqueue(new GpuProcessLiveSample(LastSeenMs, GpuPercent, DedicatedMb, SharedMb));
        long cutoff = LastSeenMs - (long)TimeSpan.FromMinutes(10).TotalMilliseconds;
        while (_samples.Count > 1 && (_samples.Peek().TimestampMs < cutoff || _samples.Count > 600))
        {
            _samples.Dequeue();
        }

        UpdateOneMinuteSummary();
        SamplesChanged?.Invoke(this);
    }

    public void MarkEnded(long timestampMs)
    {
        IsRunning = false;
        IsUsingGraphics = false;
        LastSeenMs = timestampMs > 0 ? timestampMs : DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
    }

    public static bool HasMeasuredGraphics(ProcessRow source) =>
        source.GpuPermille > 0 || source.GpuDedicatedBytes > 0 || source.GpuSharedBytes > 0;

    private void UpdateOneMinuteSummary()
    {
        long cutoff = LastSeenMs - (long)TimeSpan.FromMinutes(1).TotalMilliseconds;
        var recent = _samples.Where(sample => sample.TimestampMs >= cutoff).ToArray();
        if (recent.Length == 0)
        {
            return;
        }

        AverageGpuOneMinute = recent.Average(sample => sample.GpuPercent);
        PeakGpuOneMinute = recent.Max(sample => sample.GpuPercent);
        DedicatedDeltaOneMinuteMb = DedicatedMb - recent[0].DedicatedMb;
    }

    private static string FormatSigned(double value)
    {
        if (Math.Abs(value) < 0.05)
        {
            return "0";
        }
        return string.Create(Inv, $"{(value > 0 ? "+" : string.Empty)}{value:F1}");
    }
}

public enum GpuChangeKind
{
    TrackingStarted,
    TrackingStopped,
    WorkloadStarted,
    WorkloadStopped,
    ProcessGpuRaised,
    ProcessGpuSettled,
    AdapterGpuRaised,
    AdapterGpuSettled,
    MemoryPressure,
    MemorySettled,
    TemperatureNearLimit,
    TemperatureSettled,
    ThrottleStarted,
    ThrottleStopped,
}

public sealed class GpuChangeViewModel
{
    public DateTimeOffset Timestamp { get; }
    public GpuChangeKind Kind { get; }
    public string Title { get; }
    public string Detail { get; }
    public bool IsTracked { get; }
    public string TimeText => Timestamp.ToString("t");
    public string KindText => Kind switch
    {
        GpuChangeKind.TrackingStarted => "Tracking started",
        GpuChangeKind.TrackingStopped => "Tracking stopped",
        GpuChangeKind.WorkloadStarted => "Workload active",
        GpuChangeKind.WorkloadStopped => "Workload stopped",
        GpuChangeKind.ProcessGpuRaised => "Process GPU increase",
        GpuChangeKind.ProcessGpuSettled => "Process GPU settled",
        GpuChangeKind.AdapterGpuRaised => "Adapter load high",
        GpuChangeKind.AdapterGpuSettled => "Adapter load settled",
        GpuChangeKind.MemoryPressure => "Memory pressure",
        GpuChangeKind.MemorySettled => "Memory settled",
        GpuChangeKind.TemperatureNearLimit => "Temperature near warning",
        GpuChangeKind.TemperatureSettled => "Temperature settled",
        GpuChangeKind.ThrottleStarted => "Thermal throttle",
        _ => "Thermal throttle cleared",
    };
    public string IconGlyph => Kind switch
    {
        GpuChangeKind.TrackingStarted => "\uE735",
        GpuChangeKind.TrackingStopped => "\uE734",
        GpuChangeKind.WorkloadStarted => "\uE9D9",
        GpuChangeKind.WorkloadStopped => "\uE711",
        GpuChangeKind.ProcessGpuRaised or GpuChangeKind.AdapterGpuRaised => "\uE9D9",
        GpuChangeKind.ProcessGpuSettled or GpuChangeKind.AdapterGpuSettled => "\uE73E",
        GpuChangeKind.MemoryPressure => "\uE8C8",
        GpuChangeKind.MemorySettled => "\uE73E",
        GpuChangeKind.TemperatureNearLimit or GpuChangeKind.ThrottleStarted => "\uE7E7",
        _ => "\uE73E",
    };
    public string AccessibleSummary => $"{TimeText}, {KindText}: {Title}. {Detail}";

    public GpuChangeViewModel(
        DateTimeOffset timestamp,
        GpuChangeKind kind,
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

    public override string ToString() => AccessibleSummary;
}
