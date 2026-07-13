using Atlas.IpcClient;
using Atlas.V0;
using Xunit;

namespace Atlas.IpcClient.Tests;

public class PrivacyAlertFormatterTests
{
    // ---- Capability --------------------------------------------------------

    [Theory]
    [InlineData(CapabilityKind.Camera, "Camera")]
    [InlineData(CapabilityKind.Microphone, "Microphone")]
    [InlineData(CapabilityKind.Location, "Location")]
    [InlineData(CapabilityKind.Unspecified, "All capabilities")]
    public void CapabilityLabel_UnspecifiedMeansAll(CapabilityKind kind, string expected)
    {
        Assert.Equal(expected, PrivacyAlertFormatter.CapabilityLabel(kind));
    }

    [Fact]
    public void CapabilityGlyph_IsNonEmptyIncludingAll()
    {
        Assert.False(string.IsNullOrEmpty(PrivacyAlertFormatter.CapabilityGlyph(CapabilityKind.Camera)));
        Assert.False(string.IsNullOrEmpty(PrivacyAlertFormatter.CapabilityGlyph(CapabilityKind.Microphone)));
        Assert.False(string.IsNullOrEmpty(PrivacyAlertFormatter.CapabilityGlyph(CapabilityKind.Location)));
        Assert.False(string.IsNullOrEmpty(PrivacyAlertFormatter.CapabilityGlyph(CapabilityKind.Unspecified)));
    }

    // ---- Condition ---------------------------------------------------------

    [Theory]
    [InlineData(PrivacyAlertCondition.AlertAnyUse, "Any use")]
    [InlineData(PrivacyAlertCondition.AlertBackgroundUse, "Background use")]
    [InlineData(PrivacyAlertCondition.AlertWhileLocked, "While locked")]
    [InlineData(PrivacyAlertCondition.AlertUnknownApp, "Unknown app")]
    [InlineData(PrivacyAlertCondition.AlertLongerThan, "Active longer than…")]
    [InlineData(PrivacyAlertCondition.Unspecified, "Any use")]
    public void ConditionLabel_MapsConditions(PrivacyAlertCondition condition, string expected)
    {
        Assert.Equal(expected, PrivacyAlertFormatter.ConditionLabel(condition));
    }

    [Fact]
    public void ConditionSummary_LongerThan_FoldsInThreshold()
    {
        Assert.Equal("Active longer than 30 s",
            PrivacyAlertFormatter.ConditionSummary(PrivacyAlertCondition.AlertLongerThan, 30));
        Assert.Equal("Active longer than 5 min",
            PrivacyAlertFormatter.ConditionSummary(PrivacyAlertCondition.AlertLongerThan, 300));
    }

    [Fact]
    public void ConditionSummary_NonThresholdConditions_IgnoreThreshold()
    {
        Assert.Equal("Background use",
            PrivacyAlertFormatter.ConditionSummary(PrivacyAlertCondition.AlertBackgroundUse, 999));
    }

    [Theory]
    [InlineData(0u, "0 s")]
    [InlineData(30u, "30 s")]
    [InlineData(60u, "1 min")]
    [InlineData(90u, "1 min 30 s")]
    [InlineData(300u, "5 min")]
    [InlineData(3600u, "1 h")]
    [InlineData(5400u, "1 h 30 min")]
    public void ThresholdText_Buckets(uint seconds, string expected)
    {
        Assert.Equal(expected, PrivacyAlertFormatter.ThresholdText(seconds));
    }

    [Theory]
    [InlineData(PrivacyAlertCondition.AlertLongerThan, true)]
    [InlineData(PrivacyAlertCondition.AlertAnyUse, false)]
    [InlineData(PrivacyAlertCondition.AlertBackgroundUse, false)]
    public void ConditionUsesThreshold_OnlyLongerThan(PrivacyAlertCondition condition, bool expected)
    {
        Assert.Equal(expected, PrivacyAlertFormatter.ConditionUsesThreshold(condition));
    }

    // ---- Rule summary ------------------------------------------------------

    [Fact]
    public void RuleSummary_CapabilityAndCondition()
    {
        var rule = new PrivacyAlertRule
        {
            Capability = CapabilityKind.Camera,
            Condition = PrivacyAlertCondition.AlertBackgroundUse,
        };
        Assert.Equal("Camera • Background use", PrivacyAlertFormatter.RuleSummary(rule));
    }

    [Fact]
    public void RuleSummary_AllCapabilitiesLongerThan()
    {
        var rule = new PrivacyAlertRule
        {
            Capability = CapabilityKind.Unspecified,
            Condition = PrivacyAlertCondition.AlertLongerThan,
            ThresholdSeconds = 300,
        };
        Assert.Equal("All capabilities • Active longer than 5 min",
            PrivacyAlertFormatter.RuleSummary(rule));
    }

    [Fact]
    public void RuleName_BlankGetsPlaceholder()
    {
        Assert.Equal("(unnamed alert)", PrivacyAlertFormatter.RuleName(new PrivacyAlertRule()));
        Assert.Equal("Camera watch",
            PrivacyAlertFormatter.RuleName(new PrivacyAlertRule { Name = "Camera watch" }));
    }

    // ---- Fired alerts (factual, never accusatory) --------------------------

    [Fact]
    public void FiredAlertLine_IsFactualCapabilityAppDetailTime()
    {
        long now = 100_000_000_000;
        var alert = new FiredAlert
        {
            Capability = CapabilityKind.Microphone,
            DisplayName = "Discord",
            Detail = "background use",
            TsMs = now - 2 * 60_000, // 2 minutes ago
        };
        Assert.Equal("Microphone — Discord — background use, 2m ago",
            PrivacyAlertFormatter.FiredAlertLine(alert, now));
    }

    [Fact]
    public void AppDisplay_FallsBackToAppIdThenUnknown()
    {
        Assert.Equal("Discord",
            PrivacyAlertFormatter.AppDisplay(new FiredAlert { DisplayName = "Discord", AppId = "com.discord" }));
        Assert.Equal("com.discord",
            PrivacyAlertFormatter.AppDisplay(new FiredAlert { AppId = "com.discord" }));
        Assert.Equal("(unknown app)",
            PrivacyAlertFormatter.AppDisplay(new FiredAlert()));
    }

    [Fact]
    public void DetailText_BlankBecomesPlainUsed()
    {
        Assert.Equal("used", PrivacyAlertFormatter.DetailText(new FiredAlert()));
        Assert.Equal("while locked",
            PrivacyAlertFormatter.DetailText(new FiredAlert { Detail = "while locked" }));
    }

    [Fact]
    public void FiredAlertLine_UnknownAppAndDetail_StaysCalm()
    {
        long now = 5_000_000;
        var alert = new FiredAlert { Capability = CapabilityKind.Location, TsMs = now };
        // Never invents intent: unknown app + no detail + "just now".
        Assert.Equal("Location — (unknown app) — used, just now",
            PrivacyAlertFormatter.FiredAlertLine(alert, now));
    }
}
