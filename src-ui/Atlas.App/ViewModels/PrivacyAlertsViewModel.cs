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
/// Drives the Privacy Alerts page (R2, PRD §9.10.3): a list of alert rules over
/// camera / microphone / location usage (name, capability, condition, threshold,
/// enabled toggle) plus a "Recent alerts" log of rules that have fired. Create /
/// edit happens in <c>PrivacyAlertEditDialog</c>; this view-model owns the list,
/// the enable/disable and delete operations, and the unavailable/empty states.
///
/// <para>
/// The framing is deliberately calm and factual. An alert rule watches usage; a
/// fired alert means <b>"you asked to be told about this"</b>, never "a threat was
/// found" (PRD §9.10.3, proto R2 header). Nothing here implies malice.
/// </para>
///
/// <para>
/// The five privacy-alert RPCs land server-side after this UI, so every call
/// degrades gracefully: an <c>Unimplemented</c> reply becomes a calm "unavailable
/// — the service is too old" placeholder rather than a crash (task brief). Set
/// <c>ATLAS_FAKE_PRIVACY_ALERTS=1</c> to populate the page with sample data for
/// previewing the UX without a backend.
/// </para>
/// </summary>
public sealed partial class PrivacyAlertsViewModel : ObservableObject
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

    [ObservableProperty] private bool _alertsUnavailable;
    [ObservableProperty] private bool _alertsEmpty;
    [ObservableProperty] private string _alertsStatus = string.Empty;

    public ObservableCollection<PrivacyAlertRuleRowViewModel> Rules { get; } = new();
    public ObservableCollection<FiredAlertRowViewModel> RecentAlerts { get; } = new();

    public PrivacyAlertsViewModel(DispatcherQueue dispatcher, string? who = null, bool fake = false)
    {
        _dispatcher = dispatcher;
        _who = who;
        _fake = fake;
    }

    /// <summary>Whether this page is running against demo data (no backend).</summary>
    public bool IsFake => _fake;

    /// <summary>Loads (or reloads) the alert-rule list and the recent fired alerts.</summary>
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

        long now = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();

        try
        {
            using var channel = AtlasChannel.Connect(_who);

            var rulesOutcome = await channel.ListPrivacyAlertRulesAsync(ct).ConfigureAwait(false);
            if (ct.IsCancellationRequested)
            {
                return;
            }

            if (!rulesOutcome.Supported)
            {
                Post(() =>
                {
                    Rules.Clear();
                    IsUnavailable = true;
                    HasLoaded = true;
                    StatusText = "Privacy alerts unavailable — the connected service is too old.";
                    IsLoading = false;
                    RecentAlerts.Clear();
                    AlertsUnavailable = true;
                    AlertsEmpty = false;
                    AlertsStatus = string.Empty;
                });
                return;
            }

            // Recent fired alerts are a separate RPC; they may be supported or not
            // independently. Never let their absence blank the rules list. Look
            // back seven days — privacy alerts fire far less often than events.
            var alertsOutcome = await channel
                .ListFiredAlertsAsync(now - (long)TimeSpan.FromDays(7).TotalMilliseconds, now,
                    limit: 100, cancellationToken: ct)
                .ConfigureAwait(false);
            if (ct.IsCancellationRequested)
            {
                return;
            }

            Post(() =>
            {
                Rules.Clear();
                foreach (var r in rulesOutcome.Value.Rules)
                {
                    Rules.Add(new PrivacyAlertRuleRowViewModel(r));
                }
                IsEmpty = Rules.Count == 0;
                HasLoaded = true;
                StatusText = Rules.Count == 0
                    ? "No alert rules yet."
                    : $"{Rules.Count} alert rule{(Rules.Count == 1 ? "" : "s")}.";
                IsLoading = false;

                PopulateAlerts(alertsOutcome, now);
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
                IsUnavailable = true;
                HasLoaded = true;
                StatusText = $"Could not reach the service: {ex.Message}";
                IsLoading = false;
                RecentAlerts.Clear();
                AlertsUnavailable = true;
                AlertsStatus = string.Empty;
            });
        }
    }

    private void PopulateAlerts(RpcOutcome<ListFiredAlertsReply> outcome, long nowMs)
    {
        RecentAlerts.Clear();
        if (!outcome.Supported)
        {
            AlertsUnavailable = true;
            AlertsEmpty = false;
            AlertsStatus = string.Empty;
            return;
        }

        AlertsUnavailable = false;
        foreach (var a in outcome.Value.Alerts)
        {
            RecentAlerts.Add(new FiredAlertRowViewModel(a, nowMs));
        }
        AlertsEmpty = RecentAlerts.Count == 0;
        AlertsStatus = RecentAlerts.Count == 0
            ? "No alerts have fired recently."
            : $"{RecentAlerts.Count} alert{(RecentAlerts.Count == 1 ? "" : "s")} in the last 7 days"
                + (outcome.Value.Truncated ? " (showing the most recent)." : ".");
    }

    /// <summary>
    /// Toggles a rule's enabled state by updating it in place (there is no dedicated
    /// enable RPC — the update carries the flipped flag). Enabling starts the watch;
    /// disabling stops it. Neither changes anything on the system itself.
    /// </summary>
    public async Task<(bool ok, string message)> SetEnabledAsync(long ruleId, bool enabled)
    {
        var row = FindRow(ruleId);
        if (row is null)
        {
            return (false, "The rule is no longer in the list.");
        }

        if (_fake)
        {
            Post(() => row.ApplyEnabled(enabled));
            return (true, string.Empty);
        }

        try
        {
            var updated = row.Rule.Clone();
            updated.Enabled = enabled;

            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.UpdatePrivacyAlertRuleAsync(updated).ConfigureAwait(false);
            if (!outcome.Supported)
            {
                return (false, "This service is too old to change privacy alerts.");
            }
            if (!outcome.Value.Ok)
            {
                return (false, "The service could not change this alert rule.");
            }
            Post(() => row.ApplyEnabled(enabled));
            return (true, string.Empty);
        }
        catch (Exception ex)
        {
            return (false, ex.Message);
        }
    }

    /// <summary>Deletes an alert rule by id. The rule simply stops watching.</summary>
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
            var outcome = await channel.DeletePrivacyAlertRuleAsync(ruleId).ConfigureAwait(false);
            if (!outcome.Supported)
            {
                return (false, "This service is too old to delete privacy alerts.");
            }
            return outcome.Value.Ok
                ? (true, string.Empty)
                : (false, "The service could not delete this alert rule.");
        }
        catch (Exception ex)
        {
            return (false, ex.Message);
        }
    }

    private PrivacyAlertRuleRowViewModel? FindRow(long ruleId)
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
            foreach (var r in PrivacyAlertsDemoData.SampleRules())
            {
                Rules.Add(new PrivacyAlertRuleRowViewModel(r));
            }
            IsEmpty = Rules.Count == 0;
            IsUnavailable = false;
            HasLoaded = true;
            StatusText = $"{Rules.Count} alert rules (demo data).";
            IsLoading = false;

            RecentAlerts.Clear();
            foreach (var a in PrivacyAlertsDemoData.SampleFiredAlerts(now))
            {
                RecentAlerts.Add(new FiredAlertRowViewModel(a, now));
            }
            AlertsUnavailable = false;
            AlertsEmpty = RecentAlerts.Count == 0;
            AlertsStatus = $"{RecentAlerts.Count} alerts in the last 7 days (demo data).";
        });
    }

    public void Stop() => _cts?.Cancel();

    private void Post(Action action) => _dispatcher.TryEnqueue(() => action());
}

/// <summary>
/// One alert-rule row: the raw <see cref="PrivacyAlertRule"/> for editing plus
/// pre-formatted display strings. Observable so the enabled toggle and any edit
/// reflect live without a full reload.
/// </summary>
public sealed partial class PrivacyAlertRuleRowViewModel : ObservableObject
{
    [ObservableProperty] private bool _enabled;

    public PrivacyAlertRuleRowViewModel(PrivacyAlertRule rule)
    {
        Rule = rule;
        _enabled = rule.Enabled;
    }

    /// <summary>The underlying rule (source of truth for edit + toggle).</summary>
    public PrivacyAlertRule Rule { get; private set; }

    public long Id => Rule.Id;
    public string Name => PrivacyAlertFormatter.RuleName(Rule);
    public string CapabilityText => PrivacyAlertFormatter.CapabilityLabel(Rule.Capability);
    public string CapabilityGlyph => PrivacyAlertFormatter.CapabilityGlyph(Rule.Capability);
    public string ConditionText => PrivacyAlertFormatter.ConditionSummary(Rule.Condition, Rule.ThresholdSeconds);

    /// <summary>Replaces the underlying rule after an edit and refreshes display.</summary>
    public void ApplyRule(PrivacyAlertRule rule)
    {
        Rule = rule;
        Enabled = rule.Enabled;
        OnPropertyChanged(nameof(Name));
        OnPropertyChanged(nameof(CapabilityText));
        OnPropertyChanged(nameof(CapabilityGlyph));
        OnPropertyChanged(nameof(ConditionText));
    }

    /// <summary>Reflects a confirmed enabled-state change from the service.</summary>
    public void ApplyEnabled(bool enabled)
    {
        Rule.Enabled = enabled;
        Enabled = enabled;
    }
}

/// <summary>
/// One fired-alert row for the "Recent alerts" log, pre-formatted and strictly
/// factual: capability, app, what happened, when. No accusatory language.
/// </summary>
public sealed class FiredAlertRowViewModel
{
    public FiredAlertRowViewModel(FiredAlert alert, long nowMs)
    {
        Capability = PrivacyAlertFormatter.CapabilityLabel(alert.Capability);
        Glyph = PrivacyAlertFormatter.CapabilityGlyph(alert.Capability);
        AppName = PrivacyAlertFormatter.AppDisplay(alert);
        Detail = PrivacyAlertFormatter.DetailText(alert);
        TimeText = M7Formatter.RelativeTime(alert.TsMs, nowMs);
        RuleName = string.IsNullOrWhiteSpace(alert.RuleName) ? "an alert rule" : alert.RuleName;
        Line = PrivacyAlertFormatter.FiredAlertLine(alert, nowMs);
    }

    public string Capability { get; }
    public string Glyph { get; }
    public string AppName { get; }
    public string Detail { get; }
    public string TimeText { get; }
    public string RuleName { get; }

    /// <summary>The combined factual one-liner, reused as the row tooltip.</summary>
    public string Line { get; }
}
