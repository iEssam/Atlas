using System.Collections.Generic;
using Atlas.V0;

namespace Atlas.App.ViewModels;

/// <summary>
/// Sample privacy-alert data so the Privacy Alerts page can be previewed without
/// the backend RPCs (which land after this UI — task brief). Gated behind
/// <c>ATLAS_FAKE_PRIVACY_ALERTS=1</c>; never used against a live service. The data
/// is representative — a few realistic watch rules and a calm, factual fired-alert
/// log — so the whole UX and its non-accusatory framing can be seen end to end.
/// </summary>
internal static class PrivacyAlertsDemoData
{
    public static IReadOnlyList<PrivacyAlertRule> SampleRules() => new List<PrivacyAlertRule>
    {
        new PrivacyAlertRule
        {
            Id = 1,
            Name = "Camera in the background",
            Enabled = true,
            Capability = CapabilityKind.Camera,
            Condition = PrivacyAlertCondition.AlertBackgroundUse,
        },
        new PrivacyAlertRule
        {
            Id = 2,
            Name = "Mic while the screen is locked",
            Enabled = true,
            Capability = CapabilityKind.Microphone,
            Condition = PrivacyAlertCondition.AlertWhileLocked,
        },
        new PrivacyAlertRule
        {
            Id = 3,
            Name = "Long location sessions",
            Enabled = false,
            Capability = CapabilityKind.Location,
            Condition = PrivacyAlertCondition.AlertLongerThan,
            ThresholdSeconds = 300,
        },
        new PrivacyAlertRule
        {
            Id = 4,
            Name = "Any unknown app",
            Enabled = true,
            Capability = CapabilityKind.Unspecified,
            Condition = PrivacyAlertCondition.AlertUnknownApp,
        },
    };

    public static IReadOnlyList<FiredAlert> SampleFiredAlerts(long nowMs) => new List<FiredAlert>
    {
        new FiredAlert
        {
            Id = 101,
            RuleId = 1,
            RuleName = "Camera in the background",
            Capability = CapabilityKind.Camera,
            AppId = "Microsoft.Teams_8wekyb3d8bbwe",
            DisplayName = "Microsoft Teams",
            Detail = "background use",
            TsMs = nowMs - 8 * 60_000,
        },
        new FiredAlert
        {
            Id = 102,
            RuleId = 2,
            RuleName = "Mic while the screen is locked",
            Capability = CapabilityKind.Microphone,
            AppId = "com.discord",
            DisplayName = "Discord",
            Detail = "used while locked",
            TsMs = nowMs - 3 * 60 * 60_000L,
        },
        new FiredAlert
        {
            Id = 103,
            RuleId = 3,
            RuleName = "Long location sessions",
            Capability = CapabilityKind.Location,
            AppId = "MSWeather",
            DisplayName = "Weather",
            Detail = "active for 7 min",
            TsMs = nowMs - 26 * 60 * 60_000L,
        },
    };
}
