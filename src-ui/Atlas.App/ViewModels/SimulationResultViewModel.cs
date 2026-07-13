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
/// Backs the simulation preview (PRD §9.7.5) — the centerpiece of the Rules page.
/// Given a (possibly unsaved) rule, it calls the pure dry-run <c>SimulateRule</c>
/// and exposes the affected processes with their current→new priority/affinity
/// and eco change, plus any conflicts with other enabled rules. Protected-critical
/// targets are surfaced as <see cref="SimulatedTargetRow.Blocked"/> so the view
/// can mark them calmly and distinctly ("Atlas won't touch this"). Simulation
/// <b>never applies anything</b>; running it is always safe.
///
/// <para>
/// Reused by both the create/edit dialog (preview-before-commit) and the
/// standalone preview from the list, so the impact rendering lives in exactly one
/// place. Degrades to a calm "unavailable" state when the service is too old.
/// </para>
/// </summary>
public sealed partial class SimulationResultViewModel : ObservableObject
{
    private readonly DispatcherQueue _dispatcher;
    private readonly string? _who;
    private readonly bool _fake;
    private CancellationTokenSource? _cts;

    [ObservableProperty] private bool _isRunning;
    [ObservableProperty] private bool _hasRun;
    [ObservableProperty] private bool _isUnavailable;
    [ObservableProperty] private bool _isEmpty;
    [ObservableProperty] private bool _hasConflicts;
    [ObservableProperty] private bool _hasBlocked;
    [ObservableProperty] private string _statusText = "Preview to see exactly what would change.";
    [ObservableProperty] private string _summaryText = string.Empty;

    public ObservableCollection<SimulatedTargetRow> Targets { get; } = new();
    public ObservableCollection<string> Conflicts { get; } = new();

    public SimulationResultViewModel(DispatcherQueue dispatcher, string? who = null, bool fake = false)
    {
        _dispatcher = dispatcher;
        _who = who;
        _fake = fake;
    }

    /// <summary>
    /// Runs a dry-run preview for <paramref name="rule"/>. Safe to call repeatedly;
    /// each run supersedes the previous one. Never mutates the system.
    /// </summary>
    public async Task RunAsync(Rule rule)
    {
        _cts?.Cancel();
        var cts = new CancellationTokenSource();
        _cts = cts;
        var ct = cts.Token;

        IsRunning = true;
        IsUnavailable = false;
        IsEmpty = false;
        HasRun = true;
        StatusText = "Simulating…";
        SummaryText = string.Empty;

        if (_fake)
        {
            Populate(RulesDemoData.SampleSimulation(rule));
            return;
        }

        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.SimulateRuleAsync(rule, ct).ConfigureAwait(false);
            if (ct.IsCancellationRequested)
            {
                return;
            }

            if (!outcome.Supported)
            {
                Post(() =>
                {
                    Targets.Clear();
                    Conflicts.Clear();
                    HasConflicts = false;
                    HasBlocked = false;
                    IsUnavailable = true;
                    IsEmpty = false;
                    StatusText = "Preview unavailable — the connected service is too old to simulate rules.";
                    SummaryText = string.Empty;
                    IsRunning = false;
                });
                return;
            }

            Post(() => Populate(outcome.Value));
        }
        catch (OperationCanceledException)
        {
            // Superseded.
        }
        catch (Exception ex)
        {
            Post(() =>
            {
                Targets.Clear();
                Conflicts.Clear();
                HasConflicts = false;
                HasBlocked = false;
                IsUnavailable = true;
                StatusText = $"Could not run the preview: {ex.Message}";
                IsRunning = false;
            });
        }
    }

    private void Populate(SimulateRuleReply reply)
    {
        Targets.Clear();
        int blocked = 0;
        foreach (var t in reply.Targets)
        {
            var row = new SimulatedTargetRow(t);
            if (row.Blocked)
            {
                blocked++;
            }
            Targets.Add(row);
        }

        Conflicts.Clear();
        foreach (var c in reply.Conflicts)
        {
            Conflicts.Add(c);
        }

        HasConflicts = Conflicts.Count > 0;
        HasBlocked = blocked > 0;
        IsEmpty = Targets.Count == 0;
        IsUnavailable = false;
        SummaryText = RulesFormatter.SimulationSummary(Targets.Count, blocked);
        StatusText = SummaryText;
        IsRunning = false;
    }

    public void Stop() => _cts?.Cancel();

    private void Post(Action action) => _dispatcher.TryEnqueue(() => action());
}

/// <summary>
/// One simulated target row (pre-formatted): a process the rule would touch, its
/// current→new transitions, and — when protected — the calm blocked reason. A
/// blocked row is information framed as protection, never an error.
/// </summary>
public sealed class SimulatedTargetRow
{
    public SimulatedTargetRow(SimulatedTarget t)
    {
        Pid = t.Pid;
        PidText = $"pid {t.Pid}";
        ImageName = string.IsNullOrWhiteSpace(t.ImageName) ? "(unknown)" : t.ImageName;
        PriorityTransition = RulesFormatter.TransitionText(t.CurrentPriority, t.NewPriority);
        AffinityTransition = RulesFormatter.TransitionText(t.CurrentAffinity, t.NewAffinity);
        EcoText = RulesFormatter.EcoChangeText(t.EcoQosChange);
        Blocked = t.Blocked;
        BlockedReason = RulesFormatter.BlockedReasonText(t.BlockedReason);
    }

    public uint Pid { get; }
    public string PidText { get; }
    public string ImageName { get; }
    public string PriorityTransition { get; }
    public string AffinityTransition { get; }
    public string EcoText { get; }
    public bool Blocked { get; }
    public string BlockedReason { get; }

    /// <summary>True for a normal (non-blocked) target — drives the details column visibility.</summary>
    public bool IsActionable => !Blocked;
}
