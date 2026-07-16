using Atlas.IpcClient;
using Atlas.V0;

namespace Atlas.App.ViewModels;

/// <summary>A compact, read-only projection of one service-owned insight.</summary>
public sealed class OverviewInsightViewModel
{
    public OverviewInsightViewModel(Insight insight)
    {
        Fingerprint = insight.Fingerprint;
        Glyph = InsightFormatter.KindGlyph(insight.Kind);
        SeverityToken = M8Formatter.SeverityColorToken(insight.Severity);
        StatusText = InsightFormatter.StatusLabel(insight.Status);
        ConfidenceText = M8Formatter.ConfidenceLabel(insight.Confidence);
        Title = insight.Title;
        Observation = insight.Observation;
        Significance = insight.Significance;
        EvidenceText = InsightFormatter.EvidenceSummary(insight);
        ScopeText = insight.Limitations.Count > 0
            ? $"Scope: {insight.Limitations[0]}"
            : string.Empty;

        RecommendationText = insight.Recommendation?.Text ?? string.Empty;
        HasRecommendationText = RecommendationText.Length > 0;
        Destination = insight.Recommendation?.Destination ?? string.Empty;
        ActionText = InsightFormatter.ActionLabel(Destination);
        CanOpenEvidence = ActionText.Length > 0;

        var factor = insight.Factors.FirstOrDefault();
        FactorImageName = factor?.ImageName ?? string.Empty;
        AutomationName = $"{StatusText}. {Title}. {Observation} {Significance} " +
            $"{ConfidenceText}. {EvidenceText}";
    }

    public string Fingerprint { get; }
    public string Glyph { get; }
    public string SeverityToken { get; }
    public string StatusText { get; }
    public string ConfidenceText { get; }
    public string Title { get; }
    public string Observation { get; }
    public string Significance { get; }
    public string EvidenceText { get; }
    public string ScopeText { get; }
    public string RecommendationText { get; }
    public bool HasRecommendationText { get; }
    public string Destination { get; }
    public string ActionText { get; }
    public bool CanOpenEvidence { get; }
    public string FactorImageName { get; }
    public string AutomationName { get; }
}
