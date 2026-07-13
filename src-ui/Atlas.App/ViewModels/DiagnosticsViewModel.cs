using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Globalization;
using System.Threading;
using System.Threading.Tasks;
using Atlas.IpcClient;
using Atlas.V0;
using CommunityToolkit.Mvvm.ComponentModel;
using Microsoft.UI.Dispatching;

namespace Atlas.App.ViewModels;

/// <summary>
/// Drives the Diagnostics page (M8, PRD §9.15.2) — the milestone's centerpiece.
/// The left rail lists detected incidents over a selectable window (ListIncidents);
/// selecting one runs Diagnose and renders the full structured explanation. An
/// ad-hoc "Diagnose current window" covers the no-incident case.
///
/// <para>
/// The product's whole point is epistemic honesty (PRD §3.2, §9.16.4): when
/// Diagnose returns <c>available = false</c> the page shows the engine's stated
/// reason ("insufficient evidence for this window") as a <b>first-class</b> state,
/// never a fabricated diagnosis. And when the connected service is too old to
/// serve these RPCs at all (Unimplemented → Unsupported), every surface degrades
/// to a calm "unavailable — server too old" placeholder instead of crashing
/// (task brief §4; reuses the RpcOutcome guard).
/// </para>
/// </summary>
public sealed partial class DiagnosticsViewModel : ObservableObject
{
    private readonly DispatcherQueue _dispatcher;
    private readonly string? _who;
    private CancellationTokenSource? _incidentsCts;
    private CancellationTokenSource? _diagnoseCts;

    /// <summary>Selectable windows for incident detection + ad-hoc diagnosis.</summary>
    public IReadOnlyList<DiagnosticsWindow> Windows { get; } = new[]
    {
        new DiagnosticsWindow("Last 1 hour", TimeSpan.FromHours(1)),
        new DiagnosticsWindow("Last 6 hours", TimeSpan.FromHours(6)),
        new DiagnosticsWindow("Last 24 hours", TimeSpan.FromHours(24)),
    };

    [ObservableProperty] private DiagnosticsWindow _selectedWindow;

    // ---- Incident list state ----------------------------------------------
    [ObservableProperty] private bool _isLoadingIncidents;
    [ObservableProperty] private bool _incidentsUnavailable;
    [ObservableProperty] private bool _incidentsEmpty;
    [ObservableProperty] private string _incidentsStatus = string.Empty;

    [ObservableProperty] private IncidentRowViewModel? _selectedIncident;

    public ObservableCollection<IncidentRowViewModel> Incidents { get; } = new();

    // ---- Diagnosis panel state --------------------------------------------
    [ObservableProperty] private bool _isDiagnosing;

    /// <summary>The service is too old to diagnose at all (Unimplemented).</summary>
    [ObservableProperty] private bool _diagnosisUnsupported;

    /// <summary>Diagnose answered but declined to conclude (available = false).</summary>
    [ObservableProperty] private bool _diagnosisInsufficient;

    /// <summary>The engine's plain reason when it declines (or a transport error).</summary>
    [ObservableProperty] private string _diagnosisMessage = string.Empty;

    /// <summary>The rendered diagnosis, or null when there is nothing to show.</summary>
    [ObservableProperty] private DiagnosisViewModel? _diagnosis;

    /// <summary>True before any diagnosis has been requested this session/window.</summary>
    [ObservableProperty] private bool _diagnosisIdle = true;

    public bool HasDiagnosis => Diagnosis is not null;

    /// <summary>The incident id backing the current diagnosis (0 = ad-hoc window).</summary>
    public long CurrentIncidentId { get; private set; }
    public long CurrentFromMs { get; private set; }
    public long CurrentToMs { get; private set; }

    /// <summary>True once a diagnosis is on screen, so "Export report" can enable.</summary>
    public bool CanExportReport => HasDiagnosis;

    public DiagnosticsViewModel(DispatcherQueue dispatcher, string? who = null)
    {
        _dispatcher = dispatcher;
        _who = who;
        _selectedWindow = Windows[0];
    }

    partial void OnDiagnosisChanged(DiagnosisViewModel? value)
    {
        OnPropertyChanged(nameof(HasDiagnosis));
        OnPropertyChanged(nameof(CanExportReport));
    }

    partial void OnSelectedWindowChanged(DiagnosticsWindow value)
    {
        ClearDiagnosis();
        _ = RefreshIncidentsAsync();
    }

    partial void OnSelectedIncidentChanged(IncidentRowViewModel? value)
    {
        if (value is not null)
        {
            _ = DiagnoseAsync(value.Id, WindowFrom(), WindowTo());
        }
    }

    private long WindowTo() => DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();

    private long WindowFrom() => WindowTo() - (long)SelectedWindow.Span.TotalMilliseconds;

    /// <summary>Loads (or reloads) detected incidents for the selected window.</summary>
    public async Task RefreshIncidentsAsync()
    {
        _incidentsCts?.Cancel();
        var cts = new CancellationTokenSource();
        _incidentsCts = cts;
        var ct = cts.Token;

        var from = WindowFrom();
        var to = WindowTo();

        IsLoadingIncidents = true;
        IncidentsUnavailable = false;
        IncidentsEmpty = false;
        IncidentsStatus = "Looking for incidents…";

        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.ListIncidentsAsync(from, to, limit: 100, ct).ConfigureAwait(false);

            if (ct.IsCancellationRequested)
            {
                return;
            }

            if (!outcome.Supported)
            {
                Post(() =>
                {
                    Incidents.Clear();
                    SelectedIncident = null;
                    IncidentsUnavailable = true;
                    IncidentsStatus = "Diagnostics unavailable — the service is too old.";
                    IsLoadingIncidents = false;
                });
                return;
            }

            var now = to;
            Post(() =>
            {
                Incidents.Clear();
                foreach (var incident in outcome.Value.Incidents)
                {
                    Incidents.Add(IncidentRowViewModel.From(incident, now));
                }
                SelectedIncident = null;

                IncidentsEmpty = Incidents.Count == 0;
                IncidentsStatus = Incidents.Count == 0
                    ? "No incidents detected in this window."
                    : $"{Incidents.Count} incident{(Incidents.Count == 1 ? "" : "s")} detected"
                        + (outcome.Value.Truncated ? " (showing the most recent)." : ".");
                IsLoadingIncidents = false;
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
                Incidents.Clear();
                SelectedIncident = null;
                IncidentsUnavailable = true;
                IncidentsStatus = $"Could not reach the service: {ex.Message}";
                IsLoadingIncidents = false;
            });
        }
    }

    /// <summary>
    /// Diagnoses the ad-hoc current window (no incident). Clears the incident
    /// selection so the UI reflects that this is a range diagnosis.
    /// </summary>
    public Task DiagnoseCurrentWindowAsync()
    {
        SelectedIncident = null;
        return DiagnoseAsync(0, WindowFrom(), WindowTo());
    }

    /// <summary>
    /// Runs Diagnose for an incident id (or 0 for the ad-hoc window) and renders
    /// the result. Honors the three honest outcomes: unsupported (server too old),
    /// insufficient (engine declines with a reason), and a full diagnosis.
    /// </summary>
    private async Task DiagnoseAsync(long incidentId, long fromMs, long toMs)
    {
        _diagnoseCts?.Cancel();
        var cts = new CancellationTokenSource();
        _diagnoseCts = cts;
        var ct = cts.Token;

        CurrentIncidentId = incidentId;
        CurrentFromMs = fromMs;
        CurrentToMs = toMs;

        IsDiagnosing = true;
        DiagnosisIdle = false;
        DiagnosisUnsupported = false;
        DiagnosisInsufficient = false;
        DiagnosisMessage = string.Empty;
        Diagnosis = null;

        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.DiagnoseAsync(incidentId, fromMs, toMs, ct).ConfigureAwait(false);

            if (ct.IsCancellationRequested)
            {
                return;
            }

            if (!outcome.Supported)
            {
                Post(() =>
                {
                    DiagnosisUnsupported = true;
                    DiagnosisMessage =
                        "The connected service is too old to run diagnostics. Update the service to explain incidents.";
                    IsDiagnosing = false;
                });
                return;
            }

            var reply = outcome.Value;
            if (!reply.Available)
            {
                Post(() =>
                {
                    DiagnosisInsufficient = true;
                    DiagnosisMessage = string.IsNullOrWhiteSpace(reply.UnavailableReason)
                        ? "There isn't enough evidence to diagnose this window."
                        : reply.UnavailableReason;
                    IsDiagnosing = false;
                });
                return;
            }

            var now = WindowTo();
            Post(() =>
            {
                Diagnosis = DiagnosisViewModel.From(reply.Diagnosis, now);
                IsDiagnosing = false;
            });
        }
        catch (OperationCanceledException)
        {
            // Superseded by a newer diagnose.
        }
        catch (Exception ex)
        {
            Post(() =>
            {
                DiagnosisInsufficient = true;
                DiagnosisMessage = $"Could not reach the service: {ex.Message}";
                IsDiagnosing = false;
            });
        }
    }

    private void ClearDiagnosis()
    {
        _diagnoseCts?.Cancel();
        Diagnosis = null;
        DiagnosisUnsupported = false;
        DiagnosisInsufficient = false;
        DiagnosisMessage = string.Empty;
        IsDiagnosing = false;
        DiagnosisIdle = true;
    }

    public void Stop()
    {
        _incidentsCts?.Cancel();
        _diagnoseCts?.Cancel();
    }

    private void Post(Action action) => _dispatcher.TryEnqueue(() => action());
}

/// <summary>A selectable diagnostics window.</summary>
public sealed record DiagnosticsWindow(string Label, TimeSpan Span)
{
    public override string ToString() => Label;
}

/// <summary>One incident, pre-formatted for the left-rail list.</summary>
public sealed class IncidentRowViewModel
{
    public long Id { get; }
    public string KindGlyph { get; }
    public string KindLabel { get; }
    public string SeverityLabel { get; }
    public string SeverityToken { get; }
    public string TimeText { get; }
    public string Summary { get; }
    public string PeakText { get; }
    public bool HasPeak => PeakText.Length > 0;

    private IncidentRowViewModel(
        long id, string kindGlyph, string kindLabel, string severityLabel,
        string severityToken, string timeText, string summary, string peakText)
    {
        Id = id;
        KindGlyph = kindGlyph;
        KindLabel = kindLabel;
        SeverityLabel = severityLabel;
        SeverityToken = severityToken;
        TimeText = timeText;
        Summary = summary;
        PeakText = peakText;
    }

    public static IncidentRowViewModel From(Incident incident, long nowMs) => new(
        incident.Id,
        M8Formatter.IncidentKindGlyph(incident.Kind),
        M8Formatter.IncidentKindLabel(incident.Kind),
        M8Formatter.SeverityLabel(incident.Severity),
        M8Formatter.SeverityColorToken(incident.Severity),
        M8Formatter.IncidentWindowText(incident.StartMs, incident.EndMs, nowMs),
        string.IsNullOrWhiteSpace(incident.Summary)
            ? M8Formatter.IncidentKindLabel(incident.Kind)
            : incident.Summary,
        M8Formatter.PeakValueText(incident.Kind, incident.PeakValue));
}

/// <summary>The full structured diagnosis, projected for the sectioned layout.</summary>
public sealed class DiagnosisViewModel
{
    public string Observed { get; }
    public string WindowText { get; }
    public string OverallConfidenceLabel { get; }
    public string OverallConfidenceToken { get; }

    public IReadOnlyList<EvidenceRowViewModel> Evidence { get; }
    public IReadOnlyList<FactorRowViewModel> Factors { get; }
    public IReadOnlyList<string> Alternatives { get; }

    public string Recommendation { get; }
    public string Risk { get; }
    public string Reversibility { get; }
    public string VerificationPlan { get; }

    public bool HasEvidence => Evidence.Count > 0;
    public bool HasFactors => Factors.Count > 0;
    public bool HasAlternatives => Alternatives.Count > 0;
    public bool HasRecommendation => Recommendation.Length > 0;
    public bool HasRisk => Risk.Length > 0;
    public bool HasReversibility => Reversibility.Length > 0;
    public bool HasVerificationPlan => VerificationPlan.Length > 0;

    private DiagnosisViewModel(
        string observed, string windowText, string overallConfidenceLabel,
        string overallConfidenceToken, IReadOnlyList<EvidenceRowViewModel> evidence,
        IReadOnlyList<FactorRowViewModel> factors, IReadOnlyList<string> alternatives,
        string recommendation, string risk, string reversibility, string verificationPlan)
    {
        Observed = observed;
        WindowText = windowText;
        OverallConfidenceLabel = overallConfidenceLabel;
        OverallConfidenceToken = overallConfidenceToken;
        Evidence = evidence;
        Factors = factors;
        Alternatives = alternatives;
        Recommendation = recommendation;
        Risk = risk;
        Reversibility = reversibility;
        VerificationPlan = verificationPlan;
    }

    /// <summary>Local wall-clock "HH:mm" for a diagnosis window bound.</summary>
    private static string LocalHm(long ms) =>
        DateTimeOffset.FromUnixTimeMilliseconds(ms).ToLocalTime().ToString("HH:mm", CultureInfo.CurrentCulture);

    public static DiagnosisViewModel From(Diagnosis d, long nowMs)
    {
        var evidence = new List<EvidenceRowViewModel>();
        foreach (var e in d.Evidence)
        {
            evidence.Add(EvidenceRowViewModel.From(e));
        }

        var factors = new List<FactorRowViewModel>();
        int rank = 1;
        // The overall incident kind drives attribution phrasing ("of CPU").
        var kind = InferKind(d);
        foreach (var f in d.Factors)
        {
            factors.Add(FactorRowViewModel.From(f, rank++, kind));
        }

        var alternatives = new List<string>(d.Alternatives);

        long start = d.Range?.FromMs ?? 0;
        long end = d.Range?.ToMs ?? 0;
        var windowText = start > 0
            ? M8Formatter.WindowRangeText(start, end, nowMs, LocalHm)
            : string.Empty;

        return new DiagnosisViewModel(
            string.IsNullOrWhiteSpace(d.Observed) ? "Diagnosis" : d.Observed,
            windowText,
            M8Formatter.ConfidenceLabel(d.OverallConfidence),
            M8Formatter.ConfidenceColorToken(d.OverallConfidence),
            evidence,
            factors,
            alternatives,
            d.Recommendation ?? string.Empty,
            d.Risk ?? string.Empty,
            d.Reversibility ?? string.Empty,
            d.VerificationPlan ?? string.Empty);
    }

    /// <summary>
    /// The diagnosis message has no explicit kind field; attribution phrasing only
    /// needs the resource noun, so default to CPU (the common saturation case).
    /// Contributing factors still show the concrete process either way.
    /// </summary>
    private static IncidentKind InferKind(Diagnosis d) => IncidentKind.CpuSaturation;
}

/// <summary>One measured evidence fact.</summary>
public sealed class EvidenceRowViewModel
{
    public string Text { get; }
    public string MetricText { get; }
    public bool HasMetric => MetricText.Length > 0;

    private EvidenceRowViewModel(string text, string metricText)
    {
        Text = text;
        MetricText = metricText;
    }

    public static EvidenceRowViewModel From(EvidenceItem e) => new(
        string.IsNullOrWhiteSpace(e.Text) ? "(measured fact)" : e.Text,
        M8Formatter.EvidenceMetricText(e.Metric, e.Value));
}

/// <summary>One ranked contributing factor, with a calm confidence badge.</summary>
public sealed class FactorRowViewModel
{
    public string RankText { get; }
    public string Description { get; }
    public string ConfidenceLabel { get; }
    public string ConfidenceToken { get; }
    public string ProcessText { get; }
    public string AttributionText { get; }

    public bool HasProcess => ProcessText.Length > 0;
    public bool HasAttribution => AttributionText.Length > 0;

    private FactorRowViewModel(
        string rankText, string description, string confidenceLabel,
        string confidenceToken, string processText, string attributionText)
    {
        RankText = rankText;
        Description = description;
        ConfidenceLabel = confidenceLabel;
        ConfidenceToken = confidenceToken;
        ProcessText = processText;
        AttributionText = attributionText;
    }

    public static FactorRowViewModel From(ContributingFactor f, int rank, IncidentKind kind) => new(
        rank.ToString(CultureInfo.InvariantCulture) + ".",
        string.IsNullOrWhiteSpace(f.Description) ? "(contributing factor)" : f.Description,
        M8Formatter.ConfidenceLabel(f.Confidence),
        M8Formatter.ConfidenceColorToken(f.Confidence),
        M8Formatter.ProcessText(f.ImageName, f.Pid),
        M8Formatter.AttributionText(f.Attribution, kind));
}
