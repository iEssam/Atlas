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
/// Drives the Scheduled Tasks page (R2, PRD §9.9.2): a virtualized, filterable
/// table of Windows scheduled tasks — name, folder, enabled, triggers summary,
/// last run, next run, and a colored last-result — with a detail pane showing the
/// selected task's action (exe/args), author, and run-as-highest / idle / wake
/// flags. The filter debounces and is applied <b>server-side</b> (the proto's
/// <c>ListScheduledTasks</c> takes a substring filter). Read-only. Degrades
/// gracefully when the service is too old (Unimplemented → inline placeholder).
/// </summary>
public sealed partial class ScheduledTasksViewModel : ObservableObject
{
    /// <summary>Debounce window for filter typing before a query fires.</summary>
    private static readonly TimeSpan FilterDebounce = TimeSpan.FromMilliseconds(350);

    private readonly DispatcherQueue _dispatcher;
    private readonly string? _who;
    private readonly Func<long> _nowMs;
    private CancellationTokenSource? _cts;
    private CancellationTokenSource? _debounceCts;

    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private bool _isUnavailable;
    [ObservableProperty] private bool _hasLoaded;
    [ObservableProperty] private bool _isEmpty;
    [ObservableProperty] private string _statusText = string.Empty;

    [ObservableProperty] private string _filter = string.Empty;

    [ObservableProperty] private ScheduledTaskItem? _selectedTask;

    /// <summary>True when a row is selected, so the detail pane shows/hides.</summary>
    public bool HasSelection => SelectedTask is not null;

    public ObservableCollection<ScheduledTaskItem> Tasks { get; } = new();

    public ScheduledTasksViewModel(DispatcherQueue dispatcher, string? who = null, Func<long>? nowMs = null)
    {
        _dispatcher = dispatcher;
        _who = who;
        _nowMs = nowMs ?? (() => DateTimeOffset.UtcNow.ToUnixTimeMilliseconds());
    }

    partial void OnSelectedTaskChanged(ScheduledTaskItem? value) => OnPropertyChanged(nameof(HasSelection));

    /// <summary>Debounces filter edits so rapid typing issues a single query.</summary>
    partial void OnFilterChanged(string value)
    {
        _debounceCts?.Cancel();
        var cts = new CancellationTokenSource();
        _debounceCts = cts;
        _ = DebouncedRefreshAsync(cts.Token);
    }

    private async Task DebouncedRefreshAsync(CancellationToken ct)
    {
        try
        {
            await Task.Delay(FilterDebounce, ct).ConfigureAwait(false);
        }
        catch (OperationCanceledException)
        {
            return;
        }
        if (!ct.IsCancellationRequested)
        {
            await RefreshAsync().ConfigureAwait(false);
        }
    }

    /// <summary>Loads (or reloads) the task list for the current filter.</summary>
    public async Task RefreshAsync()
    {
        _cts?.Cancel();
        var cts = new CancellationTokenSource();
        _cts = cts;
        var ct = cts.Token;

        var filter = Filter?.Trim() ?? string.Empty;

        IsLoading = true;
        IsUnavailable = false;
        IsEmpty = false;
        StatusText = "Loading…";

        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.ListScheduledTasksAsync(filter, ct).ConfigureAwait(false);

            if (ct.IsCancellationRequested)
            {
                return;
            }

            if (!outcome.Supported)
            {
                Post(() =>
                {
                    Tasks.Clear();
                    SelectedTask = null;
                    IsUnavailable = true;
                    HasLoaded = true;
                    StatusText = string.Empty;
                    IsLoading = false;
                });
                return;
            }

            var now = _nowMs();
            Post(() =>
            {
                Tasks.Clear();
                foreach (var t in outcome.Value.Tasks)
                {
                    Tasks.Add(new ScheduledTaskItem(
                        string.IsNullOrWhiteSpace(t.Name) ? "(unnamed task)" : t.Name,
                        string.IsNullOrWhiteSpace(t.Folder) ? "\\" : t.Folder,
                        string.IsNullOrWhiteSpace(t.Path) ? t.Name : t.Path,
                        t.Enabled,
                        MonitorFormatter.TaskEnabledLabel(t.Enabled),
                        MonitorFormatter.TriggersText(t.Triggers),
                        MonitorFormatter.LastRunText(t.LastRunMs, now),
                        MonitorFormatter.NextRunText(t.NextRunMs, now),
                        MonitorFormatter.TaskLastResultText(t.LastResult),
                        MonitorFormatter.TaskLastResultToken(t.LastResult),
                        MonitorFormatter.ActionText(t.Action),
                        MonitorFormatter.AuthorText(t.Author),
                        t.RunAsHighest,
                        t.RunsOnIdle,
                        t.WakesToRun));
                }
                SelectedTask = null;

                IsEmpty = Tasks.Count == 0;
                HasLoaded = true;
                StatusText = Tasks.Count == 0
                    ? (filter.Length == 0 ? "No scheduled tasks found." : $"No tasks match “{filter}”.")
                    : $"{Tasks.Count} task{(Tasks.Count == 1 ? "" : "s")}"
                        + (filter.Length == 0 ? "." : $" matching “{filter}”.");
                IsLoading = false;
            });
        }
        catch (OperationCanceledException)
        {
            // Superseded by a newer refresh.
        }
        catch (Exception ex)
        {
            Post(() =>
            {
                Tasks.Clear();
                SelectedTask = null;
                IsUnavailable = true;
                HasLoaded = true;
                StatusText = $"Could not reach the service: {ex.Message}";
                IsLoading = false;
            });
        }
    }

    public void Stop()
    {
        _debounceCts?.Cancel();
        _cts?.Cancel();
    }

    private void Post(Action action) => _dispatcher.TryEnqueue(() => action());
}

/// <summary>One scheduled-task row, pre-formatted for the table + detail pane.</summary>
public sealed class ScheduledTaskItem
{
    public string Name { get; }
    public string Folder { get; }
    public string Path { get; }
    public bool Enabled { get; }
    public string EnabledText { get; }
    public string TriggersText { get; }
    public string LastRunText { get; }
    public string NextRunText { get; }
    public string LastResultText { get; }

    /// <summary>Calm color token for the last-result dot ("ok"/"idle"/"attention").</summary>
    public string LastResultToken { get; }

    public string ActionText { get; }
    public string AuthorText { get; }
    public bool RunAsHighest { get; }
    public bool RunsOnIdle { get; }
    public bool WakesToRun { get; }

    /// <summary>Run-level caption for the detail pane.</summary>
    public string RunLevelText => MonitorFormatter.RunLevelText(RunAsHighest);

    /// <summary>Idle-start caption.</summary>
    public string IdleText => RunsOnIdle ? "Starts on idle" : "Not idle-triggered";

    /// <summary>Wake caption.</summary>
    public string WakeText => WakesToRun ? "Wakes the computer to run" : "Does not wake the computer";

    public ScheduledTaskItem(
        string name, string folder, string path, bool enabled, string enabledText,
        string triggersText, string lastRunText, string nextRunText, string lastResultText,
        string lastResultToken, string actionText, string authorText,
        bool runAsHighest, bool runsOnIdle, bool wakesToRun)
    {
        Name = name;
        Folder = folder;
        Path = path;
        Enabled = enabled;
        EnabledText = enabledText;
        TriggersText = triggersText;
        LastRunText = lastRunText;
        NextRunText = nextRunText;
        LastResultText = lastResultText;
        LastResultToken = lastResultToken;
        ActionText = actionText;
        AuthorText = authorText;
        RunAsHighest = runAsHighest;
        RunsOnIdle = runsOnIdle;
        WakesToRun = wakesToRun;
    }
}
