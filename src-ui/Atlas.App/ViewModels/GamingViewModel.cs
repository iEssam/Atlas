using System.Collections.ObjectModel;
using Atlas.IpcClient;
using Atlas.V0;
using CommunityToolkit.Mvvm.ComponentModel;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml.Controls;

namespace Atlas.App.ViewModels;

public sealed record GamingTraceSummaryRow(
    string Time,
    string Frame,
    string Cpu,
    string Gpu,
    string Memory,
    string Temperature,
    string State);

public sealed record GamingSessionDisplay(GameSession Session, string Label)
{
    public long Id => Session.Id;
}

public sealed record GamingObjectiveOption(GamingObjective Objective, string Label, string Explanation);

public sealed class GamingFindingViewModel
{
    public GamingFinding Source { get; }
    public string Title => Source.Title;
    public string Observed => Source.Observed;
    public string WhyItMatters => Source.WhyItMatters;
    public string Evidence => Source.Evidence;
    public string Confidence => Source.Confidence;
    public string ExpectedBenefit => Source.ExpectedBenefit;
    public string Tradeoff => Source.Tradeoff;
    public string Rollback => Source.Rollback;
    public string Verification => Source.Verification;
    public string Timing => Source.Temporary
        ? $"Temporary for the game session. Restart: {Source.RestartRequirement}"
        : $"Persistent until rolled back. Restart: {Source.RestartRequirement}";
    public string Limitations => Source.Limitations.Count == 0
        ? "No additional data limitation was recorded."
        : string.Join(" ", Source.Limitations);
    public string Sources => Source.Sources.Count == 0
        ? "No external source is attached; this finding is based on local observation."
        : string.Join(" · ", Source.Sources.Select(source => $"{source.Publisher}: {source.Title} (reviewed {source.ReviewedDate})"));

    public GamingFindingViewModel(GamingFinding source) => Source = source;
}

public sealed partial class GamingViewModel : ObservableObject
{
    private readonly string? _who;
    private CancellationTokenSource? _refreshCts;
    private CancellationTokenSource? _traceCts;

    public ObservableCollection<GameInstall> Games { get; } = new();
    public ObservableCollection<GamingFact> Facts { get; } = new();
    public ObservableCollection<GamingCapability> Capabilities { get; } = new();
    public ObservableCollection<GamingFindingViewModel> Findings { get; } = new();
    public ObservableCollection<GamingSessionDisplay> Sessions { get; } = new();
    public ObservableCollection<GamingTraceBucket> Trace { get; } = new();
    public ObservableCollection<GamingTraceSummaryRow> TraceSummary { get; } = new();

    public IReadOnlyList<GamingObjectiveOption> ObjectiveOptions { get; } =
    [
        new(GamingObjective.CompetitiveLatency, "Competitive latency", "Targets the highest sustainable responsiveness and keeps frame generation out of the plan."),
        new(GamingObjective.SmoothCompetitive, "Smooth competitive", "Targets stable pacing with VRR-aware caps and sustainable latency features."),
    ];

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(HasSelectedGame))]
    [NotifyPropertyChangedFor(nameof(CanPreviewPlan))]
    private GameInstall? _selectedGame;

    [ObservableProperty] private GamingObjectiveOption _selectedObjectiveOption;
    [ObservableProperty] private GamingSessionDisplay? _selectedSession;
    [ObservableProperty] private GamingReadiness? _readiness;
    [ObservableProperty] private GamingPlan? _currentPlan;
    [ObservableProperty] private bool _isBusy;
    [ObservableProperty] private bool _isRecording;
    [ObservableProperty] private bool _isEmptyLibrary;
    [ObservableProperty] private bool _isUnsupported;
    [ObservableProperty] private bool _hasMessage;
    [ObservableProperty] private string _message = "Atlas is discovering supported game libraries.";
    [ObservableProperty] private InfoBarSeverity _messageSeverity = InfoBarSeverity.Informational;
    [ObservableProperty] private string _readinessSummary = "Select a detected game to begin the ready check.";
    [ObservableProperty] private string _coverageSummary = "Sensor coverage will appear after the ready check.";
    [ObservableProperty] private string _traceSummaryText = "No gaming session selected.";
    [ObservableProperty] private string _traceEmptyMessage = "Record or select a session to populate the synchronized trace.";
    [ObservableProperty] private string _performanceTitle = "No measured game performance";
    [ObservableProperty] private string _averageFps = "Not captured";
    [ObservableProperty] private string _onePercentLowFps = "Not captured";
    [ObservableProperty] private string _frameTimeP95 = "Not captured";
    [ObservableProperty] private string _longFrames = "Not captured";
    [ObservableProperty] private string _performanceExplanation = "Select a recording to see whether Atlas captured real game frames or system telemetry only.";
    [ObservableProperty] private string _performanceEvidence = "Atlas never estimates FPS from CPU or GPU utilization.";
    [ObservableProperty] private long _activeSessionId;

    public bool HasSelectedGame => SelectedGame is not null;
    public bool CanPreviewPlan => SelectedGame is not null && !IsBusy && !SelectedGame.Running;
    public bool CanRecord => SelectedGame is not null && !IsBusy && !IsRecording;
    public bool CanStopRecording => IsRecording && !IsBusy;
    public bool CanRollback => CurrentPlan is { Id: > 0 } && !IsBusy;
    public bool CanKeep => CurrentPlan is { Id: > 0, Status: GamingPlanStatus.Applied } && !IsBusy;
    public GamingObjective SelectedObjective => SelectedObjectiveOption.Objective;
    public string ObjectiveExplanation => SelectedObjectiveOption.Explanation;

    public GamingViewModel(DispatcherQueue dispatcher, string? who = null)
    {
        _who = who;
        _selectedObjectiveOption = ObjectiveOptions[0];
    }

    partial void OnSelectedGameChanged(GameInstall? value)
    {
        if (value is not null)
        {
            _ = RefreshReadinessAsync();
        }
    }

    partial void OnSelectedObjectiveOptionChanged(GamingObjectiveOption value)
    {
        OnPropertyChanged(nameof(SelectedObjective));
        OnPropertyChanged(nameof(ObjectiveExplanation));
        if (SelectedGame is not null)
        {
            _ = RefreshReadinessAsync();
        }
    }

    partial void OnSelectedSessionChanged(GamingSessionDisplay? value)
    {
        CancelTraceLoad();
        Trace.Clear();
        TraceSummary.Clear();
        UpdatePerformanceSummary(value?.Session);
        if (value is null)
        {
            TraceEmptyMessage = Sessions.Count == 0
                ? "No recorded sessions are available for this game yet."
                : "Select a recorded session to populate the synchronized trace.";
            TraceSummaryText = Sessions.Count == 0
                ? "Record a session while you play to capture synchronized system evidence."
                : "No gaming session selected.";
            return;
        }

        TraceEmptyMessage = $"Loading the recording from {value.Label}...";
        TraceSummaryText = "Loading synchronized system samples...";
        var cts = new CancellationTokenSource();
        _traceCts = cts;
        _ = LoadTraceAsync(value.Session, cts);
    }

    partial void OnIsBusyChanged(bool value)
    {
        OnPropertyChanged(nameof(CanPreviewPlan));
        OnPropertyChanged(nameof(CanRecord));
        OnPropertyChanged(nameof(CanStopRecording));
        OnPropertyChanged(nameof(CanRollback));
        OnPropertyChanged(nameof(CanKeep));
    }

    partial void OnIsRecordingChanged(bool value)
    {
        OnPropertyChanged(nameof(CanRecord));
        OnPropertyChanged(nameof(CanStopRecording));
    }

    partial void OnCurrentPlanChanged(GamingPlan? value)
    {
        OnPropertyChanged(nameof(CanRollback));
        OnPropertyChanged(nameof(CanKeep));
    }

    public async Task LoadAsync(bool refresh = true)
    {
        CancelRefresh();
        var cts = new CancellationTokenSource();
        _refreshCts = cts;
        await RunBusyAsync(async () =>
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.ListDetectedGamesAsync(refresh, cts.Token);
            if (!outcome.Supported)
            {
                IsUnsupported = true;
                IsEmptyLibrary = true;
                ShowMessage("Gaming Intelligence is unavailable because this Atlas service is older than the app.", InfoBarSeverity.Warning);
                return;
            }

            var previousId = SelectedGame?.Id;
            Games.Clear();
            foreach (var game in outcome.Value.Games.OrderBy(game => game.DisplayName))
            {
                Games.Add(game);
            }
            IsEmptyLibrary = Games.Count == 0;
            IsUnsupported = false;
            SelectedGame = Games.FirstOrDefault(game => game.Id == previousId) ?? Games.FirstOrDefault();
            if (IsEmptyLibrary)
            {
                ReadinessSummary = "No supported installation was found in the launcher manifests Atlas can safely inspect.";
                CoverageSummary = string.Join(" ", outcome.Value.Limitations);
                ShowMessage("No games detected. Atlas checked known launcher manifests and did not scan every file on your drives.", InfoBarSeverity.Informational);
            }
            else
            {
                ShowMessage($"Detected {Games.Count} game installation{(Games.Count == 1 ? string.Empty : "s")}. Select one to inspect its evidence.", InfoBarSeverity.Success);
            }
        });
    }

    public async Task RefreshReadinessAsync()
    {
        var game = SelectedGame;
        if (game is null) return;
        var objective = SelectedObjective;
        await RunBusyAsync(async () =>
        {
            using var channel = AtlasChannel.Connect(_who);
            var readinessTask = channel.GetGamingReadinessAsync(game.Id, objective);
            var sessionsTask = channel.ListGameSessionsAsync(game.Id);
            var readinessOutcome = await readinessTask;
            var sessionsOutcome = await sessionsTask;
            if (!readinessOutcome.Supported || !sessionsOutcome.Supported)
            {
                IsUnsupported = true;
                ShowMessage("The connected Atlas service does not provide Gaming Intelligence yet.", InfoBarSeverity.Warning);
                return;
            }
            if (!readinessOutcome.Value.Available || readinessOutcome.Value.Readiness is null)
            {
                Readiness = null;
                ShowMessage(readinessOutcome.Value.UnavailableReason, InfoBarSeverity.Warning);
                return;
            }

            Readiness = readinessOutcome.Value.Readiness;
            ReadinessSummary = Readiness.Summary;
            Replace(Facts, Readiness.Facts);
            Replace(Capabilities, Readiness.Capabilities);
            Replace(Findings, Readiness.Findings.Select(finding => new GamingFindingViewModel(finding)));
            var previousSessionId = SelectedSession?.Id;
            SelectedSession = null;
            Replace(Sessions, sessionsOutcome.Value.Sessions.Select(session => new GamingSessionDisplay(
                session,
                $"{DateTimeOffset.FromUnixTimeMilliseconds(session.StartMs).ToLocalTime():g} · {FriendlyCapture(session.CaptureQuality)}")));
            SelectedSession = Sessions.FirstOrDefault(session => session.Id == previousSessionId) ?? Sessions.FirstOrDefault();
            CoverageSummary = BuildCoverageSummary(Readiness.Capabilities);
            if (game.Running)
            {
                ShowMessage("The selected game is running. Readiness remains available, but configuration plans are locked until it closes.", InfoBarSeverity.Warning);
            }
            else if (Readiness.Limitations.Count > 0)
            {
                ShowMessage(Readiness.Limitations[0], InfoBarSeverity.Informational);
            }
            else
            {
                HasMessage = false;
            }
        });
    }

    public async Task<GamingPlan?> PreviewPlanAsync()
    {
        var game = SelectedGame;
        if (game is null) return null;
        GamingPlan? result = null;
        await RunBusyAsync(async () =>
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.PreviewGamingPlanAsync(game.Id, SelectedObjective);
            if (!outcome.Supported || !outcome.Value.Available || outcome.Value.Plan is null)
            {
                ShowMessage(outcome.Supported ? outcome.Value.UnavailableReason : "Plan preview is unavailable on this service.", InfoBarSeverity.Warning);
                return;
            }
            result = outcome.Value.Plan;
        });
        return result;
    }

    public async Task<bool> PrepareAndExecuteAsync(IEnumerable<string> selectedStepIds)
    {
        var game = SelectedGame;
        if (game is null) return false;
        var succeeded = false;
        await RunBusyAsync(async () =>
        {
            using var channel = AtlasChannel.Connect(_who);
            var prepared = await channel.PrepareGamingPlanAsync(game.Id, SelectedObjective, selectedStepIds);
            if (!prepared.Supported || !prepared.Value.Allowed)
            {
                ShowMessage(prepared.Supported ? prepared.Value.DenialReason : "Plan execution is unavailable on this service.", InfoBarSeverity.Warning);
                return;
            }
            CurrentPlan = prepared.Value.Plan;
            var executed = await channel.ExecuteGamingPlanAsync(prepared.Value.ConsentToken);
            if (!executed.Supported)
            {
                ShowMessage("The prepared plan could not be executed by this service.", InfoBarSeverity.Error);
                return;
            }
            CurrentPlan = executed.Value.Plan;
            succeeded = executed.Value.Success;
            ShowMessage(executed.Value.Message, succeeded ? InfoBarSeverity.Success : InfoBarSeverity.Error);
        });
        if (succeeded) await RefreshReadinessAsync();
        return succeeded;
    }

    public async Task RollbackAsync()
    {
        var plan = CurrentPlan;
        if (plan is null || plan.Id <= 0) return;
        await RunBusyAsync(async () =>
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.RollbackGamingPlanAsync(plan.Id);
            if (!outcome.Supported)
            {
                ShowMessage("Rollback is unavailable on this service.", InfoBarSeverity.Error);
                return;
            }
            CurrentPlan = outcome.Value.Plan;
            ShowMessage(outcome.Value.Message, outcome.Value.Success ? InfoBarSeverity.Success : InfoBarSeverity.Error);
        });
        await RefreshReadinessAsync();
    }

    public async Task KeepAsync()
    {
        var plan = CurrentPlan;
        if (plan is null || plan.Id <= 0) return;
        await RunBusyAsync(async () =>
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.KeepGamingPlanAsync(plan.Id);
            if (!outcome.Supported)
            {
                ShowMessage("Keep is unavailable on this service.", InfoBarSeverity.Error);
                return;
            }
            CurrentPlan = outcome.Value.Plan;
            ShowMessage(outcome.Value.Message, outcome.Value.Success ? InfoBarSeverity.Success : InfoBarSeverity.Warning);
        });
    }

    public async Task StartSessionAsync()
    {
        var game = SelectedGame;
        if (game is null) return;
        await RunBusyAsync(async () =>
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.StartGamingSessionAsync(game.Id, SelectedObjective, CurrentPlan?.Id ?? 0);
            if (!outcome.Supported || !outcome.Value.Started || outcome.Value.Session is null)
            {
                ShowMessage(outcome.Supported ? outcome.Value.Message : "Session recording is unavailable on this service.", InfoBarSeverity.Warning);
                return;
            }
            ActiveSessionId = outcome.Value.Session.Id;
            IsRecording = true;
            ShowMessage(outcome.Value.Message, InfoBarSeverity.Informational);
        });
    }

    public async Task StopSessionAsync()
    {
        if (ActiveSessionId <= 0) return;
        var id = ActiveSessionId;
        await RunBusyAsync(async () =>
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.StopGamingSessionAsync(id);
            if (!outcome.Supported)
            {
                ShowMessage("Session stop is unavailable on this service.", InfoBarSeverity.Error);
                return;
            }
            IsRecording = false;
            ActiveSessionId = 0;
            ShowMessage(outcome.Value.Message, outcome.Value.Stopped ? InfoBarSeverity.Success : InfoBarSeverity.Warning);
        });
        await RefreshReadinessAsync();
    }

    private async Task LoadTraceAsync(GameSession session, CancellationTokenSource cts)
    {
        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.GetGameSessionTraceAsync(session.Id, cancellationToken: cts.Token);
            if (cts.IsCancellationRequested || !ReferenceEquals(_traceCts, cts)) return;
            Trace.Clear();
            TraceSummary.Clear();
            if (!outcome.Supported || !outcome.Value.Found)
            {
                TraceSummaryText = "The selected trace is unavailable.";
                TraceEmptyMessage = "Atlas could not find retained samples for this recording.";
                return;
            }
            foreach (var bucket in outcome.Value.Buckets) Trace.Add(bucket);
            var stride = Math.Max(1, Trace.Count / 8);
            for (var index = 0; index < Trace.Count; index += stride)
            {
                var bucket = Trace[index];
                TraceSummary.Add(new GamingTraceSummaryRow(
                    DateTimeOffset.FromUnixTimeMilliseconds(bucket.TsMs).ToLocalTime().ToString("T"),
                    bucket.FrameTimeMs > 0 ? $"{bucket.FrameTimeMs:F1} ms p95" : "Not captured",
                    $"{bucket.CpuPercent:F1}%",
                    $"{bucket.GpuPercent:F1}%",
                    FormatBytes(bucket.RamUsedBytes),
                    bucket.TemperatureC > 0 ? $"{bucket.TemperatureC:F0} °C" : "Not reported",
                    bucket.DataGap ? "Data gap" : string.IsNullOrWhiteSpace(bucket.EventLabel) ? "Measured" : bucket.EventLabel));
            }
            if (Trace.Count == 0)
            {
                TraceEmptyMessage = "This recording contains no samples. Record for at least a few seconds before stopping.";
                TraceSummaryText = "No synchronized system samples were retained for this recording.";
            }
            else
            {
                TraceEmptyMessage = string.Empty;
                var frameBuckets = Trace.Count(bucket => bucket.FrameTimeMs > 0);
                TraceSummaryText = frameBuckets > 0
                    ? $"{Trace.Count} synchronized system samples; {frameBuckets} seconds include measured frame-time evidence."
                    : $"{Trace.Count} synchronized system samples. This recording contains no measured game frames.";
            }
        }
        catch (OperationCanceledException)
        {
        }
        catch (Exception ex)
        {
            if (!ReferenceEquals(_traceCts, cts)) return;
            TraceEmptyMessage = "Atlas could not load this recording. Try selecting it again.";
            TraceSummaryText = "The selected trace could not be loaded.";
            ShowMessage($"Atlas could not load the gaming trace: {ex.Message}", InfoBarSeverity.Error);
        }
        finally
        {
            if (ReferenceEquals(_traceCts, cts))
            {
                _traceCts = null;
                cts.Dispose();
            }
        }
    }

    public void Stop()
    {
        CancelRefresh();
        CancelTraceLoad();
    }

    private async Task RunBusyAsync(Func<Task> work)
    {
        IsBusy = true;
        try
        {
            await work();
        }
        catch (OperationCanceledException)
        {
        }
        catch (Exception ex)
        {
            ShowMessage($"Atlas could not complete the gaming request: {ex.Message}", InfoBarSeverity.Error);
        }
        finally
        {
            IsBusy = false;
        }
    }

    private void CancelRefresh()
    {
        _refreshCts?.Cancel();
        _refreshCts?.Dispose();
        _refreshCts = null;
    }

    private void CancelTraceLoad()
    {
        _traceCts?.Cancel();
        _traceCts?.Dispose();
        _traceCts = null;
    }

    private void ShowMessage(string message, InfoBarSeverity severity)
    {
        Message = string.IsNullOrWhiteSpace(message) ? "Atlas returned no additional detail." : message;
        MessageSeverity = severity;
        HasMessage = true;
    }

    private static void Replace<T>(ObservableCollection<T> target, IEnumerable<T> values)
    {
        target.Clear();
        foreach (var value in values) target.Add(value);
    }

    private static string BuildCoverageSummary(IEnumerable<GamingCapability> capabilities)
    {
        var values = capabilities.ToArray();
        var available = values.Count(capability => capability.State == GamingCapabilityState.Available);
        var limited = values.Count(capability => capability.State is GamingCapabilityState.Limited or GamingCapabilityState.ValidationRequired);
        return $"{available} collectors available; {limited} limited or awaiting validation. Open any finding to see exactly what Atlas could not observe.";
    }

    private static string FormatBytes(ulong bytes) => bytes == 0 ? "Not reported" : $"{bytes / 1_073_741_824.0:F1} GB";

    private void UpdatePerformanceSummary(GameSession? session)
    {
        var summary = session?.Summary;
        if (session is null)
        {
            PerformanceTitle = "No measured game performance";
            AverageFps = OnePercentLowFps = FrameTimeP95 = LongFrames = "Not captured";
            PerformanceExplanation = "Select a recording to see whether Atlas captured real game frames or system telemetry only.";
            PerformanceEvidence = "Atlas never estimates FPS from CPU or GPU utilization.";
            return;
        }

        if (summary is null || summary.AverageFps <= 0)
        {
            PerformanceTitle = "System evidence only";
            AverageFps = OnePercentLowFps = FrameTimeP95 = LongFrames = "Not captured";
            PerformanceExplanation = "Atlas recorded CPU, GPU, memory, temperature, disk, and process activity, but no usable displayed-frame samples were attached. This session cannot describe FPS or pacing.";
            PerformanceEvidence = session.Limitations.Count == 0
                ? "No frame-capture limitation was returned."
                : string.Join(" ", session.Limitations);
            return;
        }

        PerformanceTitle = session.CaptureQuality == GamingCaptureQuality.FrameTimeValidated
            ? "Validated game performance"
            : "Measured game performance - diagnostic";
        AverageFps = $"{summary.AverageFps:F1} FPS";
        OnePercentLowFps = $"{summary.OnePercentLowFps:F1} FPS";
        FrameTimeP95 = $"{summary.FrameTimeP95Ms:F1} ms";
        LongFrames = $"{summary.LongFrameCount} at 50+ ms";

        var lowRatio = summary.AverageFps > 0 ? summary.OnePercentLowFps / summary.AverageFps : 1.0;
        if (summary.LongFrameCount > 0)
        {
            PerformanceExplanation = $"Atlas measured {summary.LongFrameCount} long frame{(summary.LongFrameCount == 1 ? string.Empty : "s")} at 50 ms or more. These are visible hitch candidates even if the average FPS looks high. Select the matching peaks in the trace and compare them with GPU, CPU, memory, temperature, disk, and background activity.";
        }
        else if (lowRatio < 0.75)
        {
            PerformanceExplanation = $"The 1% low is {Math.Round((1.0 - lowRatio) * 100):F0}% below the average. Frame delivery was less consistent during the slowest moments, so the average FPS alone overstates how smooth this session felt.";
        }
        else
        {
            PerformanceExplanation = "The slowest 1% stayed reasonably close to the average and no 50 ms long frames were measured. This is a pacing observation for this recording, not proof that a setting improved performance.";
        }
        PerformanceEvidence = session.CaptureQuality == GamingCaptureQuality.FrameTimeValidated
            ? "Frame evidence passed the current validation gate and can be used in matched comparisons."
            : "Frames came from process-bound PresentMon ETW with no injection. This build keeps the result diagnostic until anti-cheat compatibility and capture overhead pass the release gate.";
    }

    private static string FriendlyCapture(GamingCaptureQuality quality) => quality switch
    {
        GamingCaptureQuality.FrameTimeValidated => "validated frame and system evidence",
        GamingCaptureQuality.SystemOnly => "system evidence only",
        GamingCaptureQuality.Partial => "partial evidence",
        _ => "capture quality unavailable",
    };
}
