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
/// Drives the Rules &amp; Optimization page (R2, PRD §9.7): the list of
/// performance rules (name, match, trigger, action summary, enabled toggle) plus
/// the "Active interventions" transparency surface — what Atlas is currently
/// applying, by which rule, since when (PRD §9.7.3). Create/edit happens in
/// <c>RuleEditDialog</c> (with an inline simulation preview, PRD §9.7.5); this
/// view-model owns the list, the enable/disable and delete operations, and both
/// unavailable/empty states.
///
/// <para>
/// AtlasRules is a NEW service that lands server-side after this UI, so every
/// call degrades gracefully: an <c>Unimplemented</c> reply becomes a calm
/// "unavailable — the service is too old" placeholder rather than a crash (task
/// brief). Set <c>ATLAS_FAKE_RULES=1</c> to populate the page with sample data
/// for previewing the UX without a backend.
/// </para>
/// </summary>
public sealed partial class RulesViewModel : ObservableObject
{
    private readonly DispatcherQueue _dispatcher;
    private readonly string? _who;
    private readonly bool _fake;
    private CancellationTokenSource? _cts;

    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private bool _isUnavailable;
    [ObservableProperty] private bool _hasLoaded;
    [ObservableProperty] private bool _isEmpty;
    [ObservableProperty] private string _statusText = string.Empty;
    [ObservableProperty] private RuleRowViewModel? _selectedRule;

    [ObservableProperty] private bool _interventionsUnavailable;
    [ObservableProperty] private bool _interventionsEmpty;
    [ObservableProperty] private string _interventionsStatus = string.Empty;

    public ObservableCollection<RuleRowViewModel> Rules { get; } = new();
    public ObservableCollection<InterventionRowViewModel> Interventions { get; } = new();

    public RulesViewModel(DispatcherQueue dispatcher, string? who = null, bool fake = false)
    {
        _dispatcher = dispatcher;
        _who = who;
        _fake = fake;
    }

    /// <summary>Whether this page is running against demo data (no backend).</summary>
    public bool IsFake => _fake;

    /// <summary>Loads (or reloads) the rule list and the active interventions.</summary>
    public async Task RefreshAsync()
    {
        _cts?.Cancel();
        var cts = new CancellationTokenSource();
        _cts = cts;
        var ct = cts.Token;

        IsLoading = true;
        IsUnavailable = false;
        IsEmpty = false;
        StatusText = "Loading…";

        if (_fake)
        {
            LoadFake();
            return;
        }

        try
        {
            using var channel = AtlasChannel.Connect(_who);

            var rulesOutcome = await channel.ListRulesAsync(ct).ConfigureAwait(false);
            if (ct.IsCancellationRequested)
            {
                return;
            }

            if (!rulesOutcome.Supported)
            {
                Post(() =>
                {
                    Rules.Clear();
                    SelectedRule = null;
                    IsUnavailable = true;
                    HasLoaded = true;
                    StatusText = "Rules unavailable — the connected service is too old.";
                    IsLoading = false;
                    Interventions.Clear();
                    InterventionsUnavailable = true;
                    InterventionsEmpty = false;
                    InterventionsStatus = string.Empty;
                });
                return;
            }

            // Interventions are a separate RPC; they may be supported or not
            // independently. Never let their absence blank the rules list.
            var intvOutcome = await channel.ListInterventionsAsync(ct).ConfigureAwait(false);
            if (ct.IsCancellationRequested)
            {
                return;
            }

            long now = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();

            Post(() =>
            {
                Rules.Clear();
                foreach (var r in rulesOutcome.Value.Rules)
                {
                    Rules.Add(new RuleRowViewModel(r));
                }
                SelectedRule = null;
                IsEmpty = Rules.Count == 0;
                HasLoaded = true;
                StatusText = Rules.Count == 0
                    ? "No rules yet."
                    : $"{Rules.Count} rule{(Rules.Count == 1 ? "" : "s")}.";
                IsLoading = false;

                PopulateInterventions(intvOutcome, now);
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
                Rules.Clear();
                SelectedRule = null;
                IsUnavailable = true;
                HasLoaded = true;
                StatusText = $"Could not reach the service: {ex.Message}";
                IsLoading = false;
                Interventions.Clear();
                InterventionsUnavailable = true;
                InterventionsStatus = string.Empty;
            });
        }
    }

    private void PopulateInterventions(RpcOutcome<ListInterventionsReply> outcome, long nowMs)
    {
        Interventions.Clear();
        if (!outcome.Supported)
        {
            InterventionsUnavailable = true;
            InterventionsEmpty = false;
            InterventionsStatus = string.Empty;
            return;
        }

        InterventionsUnavailable = false;
        foreach (var i in outcome.Value.Interventions)
        {
            Interventions.Add(new InterventionRowViewModel(i, nowMs));
        }
        InterventionsEmpty = Interventions.Count == 0;
        InterventionsStatus = Interventions.Count == 0
            ? "Atlas isn't changing any processes right now."
            : $"Atlas is currently adjusting {Interventions.Count} process"
                + (Interventions.Count == 1 ? "." : "es.");
    }

    /// <summary>
    /// Toggles a rule's enabled state through the service, then reflects the
    /// server's answer on the row. Enabling applies the policy; disabling reverts
    /// it — this is the consent gesture for a persistent rule.
    /// </summary>
    public async Task<(bool ok, string message)> SetEnabledAsync(long ruleId, bool enabled)
    {
        if (_fake)
        {
            var row = FindRow(ruleId);
            row?.ApplyEnabled(enabled);
            await RefreshInterventionsOnlyAsync().ConfigureAwait(false);
            return (true, string.Empty);
        }

        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.SetRuleEnabledAsync(ruleId, enabled).ConfigureAwait(false);
            if (!outcome.Supported)
            {
                return (false, "This service is too old to change rules.");
            }
            if (!outcome.Value.Ok)
            {
                return (false, "The service could not change this rule.");
            }
            var row = FindRow(ruleId);
            Post(() => row?.ApplyEnabled(enabled));
            return (true, string.Empty);
        }
        catch (Exception ex)
        {
            return (false, ex.Message);
        }
    }

    /// <summary>Deletes a rule by id. Deleting reverts anything it was applying.</summary>
    public async Task<(bool ok, string message)> DeleteRuleAsync(long ruleId)
    {
        if (_fake)
        {
            var row = FindRow(ruleId);
            if (row is not null)
            {
                Post(() => Rules.Remove(row));
            }
            return (true, string.Empty);
        }

        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.DeleteRuleAsync(ruleId).ConfigureAwait(false);
            if (!outcome.Supported)
            {
                return (false, "This service is too old to delete rules.");
            }
            return outcome.Value.Ok ? (true, string.Empty) : (false, "The service could not delete this rule.");
        }
        catch (Exception ex)
        {
            return (false, ex.Message);
        }
    }

    private async Task RefreshInterventionsOnlyAsync()
    {
        if (_fake)
        {
            return;
        }
        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.ListInterventionsAsync().ConfigureAwait(false);
            long now = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
            Post(() => PopulateInterventions(outcome, now));
        }
        catch
        {
            // Best-effort refresh of the transparency surface.
        }
    }

    private RuleRowViewModel? FindRow(long ruleId)
    {
        foreach (var r in Rules)
        {
            if (r.Id == ruleId)
            {
                return r;
            }
        }
        return null;
    }

    private void LoadFake()
    {
        long now = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
        Post(() =>
        {
            Rules.Clear();
            foreach (var r in RulesDemoData.SampleRules())
            {
                Rules.Add(new RuleRowViewModel(r));
            }
            SelectedRule = null;
            IsEmpty = Rules.Count == 0;
            IsUnavailable = false;
            HasLoaded = true;
            StatusText = $"{Rules.Count} rules (demo data).";
            IsLoading = false;

            Interventions.Clear();
            foreach (var i in RulesDemoData.SampleInterventions(now))
            {
                Interventions.Add(new InterventionRowViewModel(i, now));
            }
            InterventionsUnavailable = false;
            InterventionsEmpty = Interventions.Count == 0;
            InterventionsStatus = $"Atlas is currently adjusting {Interventions.Count} processes (demo data).";
        });
    }

    public void Stop() => _cts?.Cancel();

    private void Post(Action action) => _dispatcher.TryEnqueue(() => action());
}

/// <summary>
/// One rule row: the raw <see cref="Rule"/> for editing/simulation plus
/// pre-formatted display strings. Observable so the enabled toggle and any edit
/// reflect live without a full reload.
/// </summary>
public sealed partial class RuleRowViewModel : ObservableObject
{
    [ObservableProperty] private bool _enabled;

    public RuleRowViewModel(Rule rule)
    {
        Rule = rule;
        _enabled = rule.Enabled;
    }

    /// <summary>The underlying rule (source of truth for edit + simulate).</summary>
    public Rule Rule { get; private set; }

    public long Id => Rule.Id;
    public string Name => string.IsNullOrWhiteSpace(Rule.Name) ? "(unnamed rule)" : Rule.Name;
    public string MatchImage => string.IsNullOrWhiteSpace(Rule.MatchImage) ? "(no match)" : Rule.MatchImage;
    public string TriggerText => RulesFormatter.RuleTriggerText(Rule.Trigger);
    public string ActionSummary => RulesFormatter.RuleActionSummary(Rule.Action);
    public string Precedence => Rule.Precedence.ToString(System.Globalization.CultureInfo.InvariantCulture);

    /// <summary>Replaces the underlying rule after an edit and refreshes display.</summary>
    public void ApplyRule(Rule rule)
    {
        Rule = rule;
        Enabled = rule.Enabled;
        OnPropertyChanged(nameof(Name));
        OnPropertyChanged(nameof(MatchImage));
        OnPropertyChanged(nameof(TriggerText));
        OnPropertyChanged(nameof(ActionSummary));
        OnPropertyChanged(nameof(Precedence));
    }

    /// <summary>Reflects a confirmed enabled-state change from the service.</summary>
    public void ApplyEnabled(bool enabled)
    {
        Rule.Enabled = enabled;
        Enabled = enabled;
    }
}

/// <summary>One active-intervention row for the transparency list (pre-formatted).</summary>
public sealed class InterventionRowViewModel
{
    public InterventionRowViewModel(Intervention intervention, long nowMs)
    {
        ImageName = string.IsNullOrWhiteSpace(intervention.ImageName) ? "(unknown)" : intervention.ImageName;
        PidText = $"pid {intervention.Pid}";
        Applied = RulesFormatter.InterventionApplied(intervention.Applied);
        RuleName = string.IsNullOrWhiteSpace(intervention.RuleName) ? "a rule" : intervention.RuleName;
        Since = RulesFormatter.RelativeSince(intervention.SinceMs, nowMs);
    }

    public string ImageName { get; }
    public string PidText { get; }
    public string Applied { get; }
    public string RuleName { get; }
    public string Since { get; }
}
