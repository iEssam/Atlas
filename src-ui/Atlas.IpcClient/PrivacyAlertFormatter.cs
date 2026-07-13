using System.Globalization;
using Atlas.V0;

namespace Atlas.IpcClient;

/// <summary>
/// Pure mapping/formatting helpers for the R2 advanced-privacy-alerts surface
/// (alert rules over camera / microphone / location, plus the fired-alert log,
/// PRD §9.10.3). Free of I/O and of any WinUI type so the view-models stay thin
/// and the logic is unit-testable without a live server (task brief §1).
///
/// <para>
/// Tone is the whole point here. A privacy alert means <b>"you asked to be told
/// about this"</b>, never "a threat was found": the fired-alert lines are strictly
/// factual — capability, app, what happened, when — and never editorialize about
/// intent (PRD §9.10.3, proto R2 header "Never implies malicious behavior —
/// factual only"). The condition/capability labels describe a <em>watch</em>, not
/// an accusation.
/// </para>
/// </summary>
public static class PrivacyAlertFormatter
{
    private static readonly CultureInfo Inv = CultureInfo.InvariantCulture;

    // ----------------------------------------------------------------------
    // Capability (a rule's UNSPECIFIED capability means "all", not "other").
    // ----------------------------------------------------------------------

    /// <summary>
    /// A capability's label for the alert surface. Unlike the general privacy
    /// page, <c>Unspecified</c> here means the rule watches <b>all</b> capabilities
    /// (camera + microphone + location), so it maps to "All capabilities" rather
    /// than "Other".
    /// </summary>
    public static string CapabilityLabel(CapabilityKind capability) => capability switch
    {
        CapabilityKind.Camera => "Camera",
        CapabilityKind.Microphone => "Microphone",
        CapabilityKind.Location => "Location",
        _ => "All capabilities",
    };

    /// <summary>
    /// A Segoe Fluent glyph for a capability. A specific capability reuses the
    /// Privacy page's glyph; the "all" case gets the privacy shield so an
    /// all-capability rule reads distinctly in the list.
    /// </summary>
    public static string CapabilityGlyph(CapabilityKind capability) => capability switch
    {
        CapabilityKind.Camera => "",       // Video
        CapabilityKind.Microphone => "",   // Microphone
        CapabilityKind.Location => "",     // MapPin / Location
        _ => "",                            // Shield (all capabilities)
    };

    // ----------------------------------------------------------------------
    // Condition.
    // ----------------------------------------------------------------------

    /// <summary>
    /// The base friendly label for an alert condition, without a threshold value:
    /// "Any use", "Background use", "While locked", "Unknown app", "Active longer
    /// than…". A missing/unspecified condition reads as "Any use" (the safest,
    /// broadest watch).
    /// </summary>
    public static string ConditionLabel(PrivacyAlertCondition condition) => condition switch
    {
        PrivacyAlertCondition.AlertAnyUse => "Any use",
        PrivacyAlertCondition.AlertBackgroundUse => "Background use",
        PrivacyAlertCondition.AlertWhileLocked => "While locked",
        PrivacyAlertCondition.AlertUnknownApp => "Unknown app",
        PrivacyAlertCondition.AlertLongerThan => "Active longer than…",
        _ => "Any use",
    };

    /// <summary>
    /// A one-line condition summary that folds in the threshold for
    /// <c>ALERT_LONGER_THAN</c>, e.g. "Active longer than 30 s" or "Active longer
    /// than 5 min". Other conditions ignore the threshold and return
    /// <see cref="ConditionLabel(PrivacyAlertCondition)"/>.
    /// </summary>
    public static string ConditionSummary(PrivacyAlertCondition condition, uint thresholdSeconds)
    {
        if (condition == PrivacyAlertCondition.AlertLongerThan)
        {
            return "Active longer than " + ThresholdText(thresholdSeconds);
        }
        return ConditionLabel(condition);
    }

    /// <summary>
    /// A friendly threshold duration for <c>ALERT_LONGER_THAN</c>: "30 s",
    /// "5 min", "1 min 30 s", "2 h". Zero renders as "0 s".
    /// </summary>
    public static string ThresholdText(uint seconds)
    {
        if (seconds == 0)
        {
            return "0 s";
        }
        if (seconds < 60)
        {
            return string.Format(Inv, "{0} s", seconds);
        }

        uint minutes = seconds / 60;
        uint remSeconds = seconds % 60;
        if (minutes < 60)
        {
            return remSeconds == 0
                ? string.Format(Inv, "{0} min", minutes)
                : string.Format(Inv, "{0} min {1} s", minutes, remSeconds);
        }

        uint hours = minutes / 60;
        uint remMinutes = minutes % 60;
        return remMinutes == 0
            ? string.Format(Inv, "{0} h", hours)
            : string.Format(Inv, "{0} h {1} min", hours, remMinutes);
    }

    /// <summary>Whether a condition uses the threshold-seconds field (drives the form).</summary>
    public static bool ConditionUsesThreshold(PrivacyAlertCondition condition) =>
        condition == PrivacyAlertCondition.AlertLongerThan;

    // ----------------------------------------------------------------------
    // Rule summary.
    // ----------------------------------------------------------------------

    /// <summary>
    /// A one-line summary of a rule's <em>watch</em> — capability plus condition —
    /// e.g. "Camera • Background use" or "All capabilities • Active longer than
    /// 5 min". Descriptive, never accusatory.
    /// </summary>
    public static string RuleSummary(PrivacyAlertRule rule) => string.Format(
        Inv,
        "{0} • {1}",
        CapabilityLabel(rule.Capability),
        ConditionSummary(rule.Condition, rule.ThresholdSeconds));

    /// <summary>A rule's display name, or a calm placeholder when blank.</summary>
    public static string RuleName(PrivacyAlertRule rule) =>
        string.IsNullOrWhiteSpace(rule.Name) ? "(unnamed alert)" : rule.Name;

    // ----------------------------------------------------------------------
    // Fired alerts (the log — strictly factual).
    // ----------------------------------------------------------------------

    /// <summary>
    /// The app display for a fired alert: the friendly display name, falling back
    /// to the raw app id, then to "(unknown app)". A blank name is not an
    /// accusation — it just means Atlas couldn't resolve a friendly label.
    /// </summary>
    public static string AppDisplay(FiredAlert alert)
    {
        if (!string.IsNullOrWhiteSpace(alert.DisplayName))
        {
            return alert.DisplayName;
        }
        return string.IsNullOrWhiteSpace(alert.AppId) ? "(unknown app)" : alert.AppId;
    }

    /// <summary>
    /// The factual detail for a fired alert (e.g. "background use while locked"),
    /// or a plain "used" fallback when the service supplied none. Never accusatory.
    /// </summary>
    public static string DetailText(FiredAlert alert) =>
        string.IsNullOrWhiteSpace(alert.Detail) ? "used" : alert.Detail;

    /// <summary>
    /// A single-line, factual fired-alert phrase, e.g.
    /// "Microphone — Discord — background use, 2m ago". It states what was accessed,
    /// by which app, the factual detail, and how long ago — nothing more. This is
    /// the tone-critical string the recent-alerts list and any tooltip reuse
    /// (task brief §1; PRD §9.10.3).
    /// </summary>
    public static string FiredAlertLine(FiredAlert alert, long nowMs) => string.Format(
        Inv,
        "{0} — {1} — {2}, {3}",
        CapabilityLabel(alert.Capability),
        AppDisplay(alert),
        DetailText(alert),
        M7Formatter.RelativeTime(alert.TsMs, nowMs));
}
