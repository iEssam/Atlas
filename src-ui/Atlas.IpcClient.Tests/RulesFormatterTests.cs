using Atlas.IpcClient;
using Atlas.V0;
using Xunit;

namespace Atlas.IpcClient.Tests;

public class RulesFormatterTests
{
    // ---- Enum labels -------------------------------------------------------

    [Theory]
    [InlineData(PriorityClass.PriorityIdle, "Idle")]
    [InlineData(PriorityClass.PriorityBelowNormal, "Below Normal")]
    [InlineData(PriorityClass.PriorityNormal, "Normal")]
    [InlineData(PriorityClass.PriorityAboveNormal, "Above Normal")]
    [InlineData(PriorityClass.PriorityHigh, "High")]
    [InlineData(PriorityClass.Unspecified, "Unchanged")]
    public void PriorityClassLabel_MapsClasses(PriorityClass priority, string expected)
    {
        Assert.Equal(expected, RulesFormatter.PriorityClassLabel(priority));
    }

    [Theory]
    [InlineData(CoreAffinityMode.AllCores, "All cores")]
    [InlineData(CoreAffinityMode.PreferPCores, "P-cores")]
    [InlineData(CoreAffinityMode.PreferECores, "E-cores")]
    [InlineData(CoreAffinityMode.CustomMask, "Custom cores")]
    [InlineData(CoreAffinityMode.CoreAffinityUnspecified, "Unchanged")]
    public void CoreAffinityModeLabel_MapsModes(CoreAffinityMode mode, string expected)
    {
        Assert.Equal(expected, RulesFormatter.CoreAffinityModeLabel(mode));
    }

    [Theory]
    [InlineData(RuleTrigger.WhileRunning, "while running")]
    [InlineData(RuleTrigger.OnAcPower, "on AC power")]
    [InlineData(RuleTrigger.OnDcPower, "on battery")]
    [InlineData(RuleTrigger.OnFullscreen, "in fullscreen apps")]
    [InlineData(RuleTrigger.Unspecified, "while running")]
    public void RuleTriggerText_MapsTriggers(RuleTrigger trigger, string expected)
    {
        Assert.Equal(expected, RulesFormatter.RuleTriggerText(trigger));
    }

    [Theory]
    [InlineData("", "No power-mode change")]
    [InlineData(null, "No power-mode change")]
    [InlineData("PowerSaver", "Power saver")]
    [InlineData("Balanced", "Balanced")]
    [InlineData("HighPerformance", "High performance")]
    [InlineData("Custom", "Custom")]
    public void PowerModeLabel_MapsModes(string? mode, string expected)
    {
        Assert.Equal(expected, RulesFormatter.PowerModeLabel(mode));
    }

    // ---- Affinity masks ----------------------------------------------------

    [Theory]
    [InlineData(0UL, "no cores")]
    [InlineData(0b1UL, "cores 0")]
    [InlineData(0b1000UL, "cores 3")]
    [InlineData(0b1111UL, "cores 0-3")]
    [InlineData(0b1_0000_1111UL, "cores 0-3,8")]
    [InlineData(0b1010UL, "cores 1,3")]
    [InlineData(0b1_0110UL, "cores 1-2,4")]
    public void AffinityMaskText_CompactsRuns(ulong mask, string expected)
    {
        Assert.Equal(expected, RulesFormatter.AffinityMaskText(mask));
    }

    [Fact]
    public void AffinityMaskText_HandlesHighBit()
    {
        Assert.Equal("cores 63", RulesFormatter.AffinityMaskText(1UL << 63));
    }

    // ---- Core-list parsing (inverse of AffinityMaskText) -------------------

    [Theory]
    [InlineData("0", 0b1UL)]
    [InlineData("0-3", 0b1111UL)]
    [InlineData("0-3,8", 0b1_0000_1111UL)]
    [InlineData("8,0-3", 0b1_0000_1111UL)]
    [InlineData(" 1 , 3 ", 0b1010UL)]
    [InlineData("63", 1UL << 63)]
    public void TryParseCoreList_ParsesValidLists(string text, ulong expected)
    {
        Assert.True(RulesFormatter.TryParseCoreList(text, out var mask));
        Assert.Equal(expected, mask);
    }

    [Theory]
    [InlineData("")]
    [InlineData("   ")]
    [InlineData(null)]
    [InlineData("64")]        // out of range (valid is 0..63)
    [InlineData("-1")]        // leading dash -> empty low token
    [InlineData("abc")]
    [InlineData("3-1")]       // reversed range
    public void TryParseCoreList_RejectsMalformed(string? text)
    {
        Assert.False(RulesFormatter.TryParseCoreList(text, out var mask));
        Assert.Equal(0UL, mask);
    }

    [Fact]
    public void TryParseCoreList_SkipsEmptyTokens()
    {
        Assert.True(RulesFormatter.TryParseCoreList("0,,2", out var mask));
        Assert.Equal(0b101UL, mask);
    }

    [Fact]
    public void TryParseCoreList_RoundTripsWithAffinityMaskText()
    {
        Assert.True(RulesFormatter.TryParseCoreList("0-3,8", out var mask));
        Assert.Equal("cores 0-3,8", RulesFormatter.AffinityMaskText(mask));
    }

    // ---- Action / rule summaries -------------------------------------------

    [Fact]
    public void RuleActionSummary_ListsOnlyChangedParts()
    {
        var action = new RuleAction
        {
            Priority = PriorityClass.PriorityBelowNormal,
            AffinityMode = CoreAffinityMode.PreferECores,
            EcoQos = true,
        };
        Assert.Equal("Below Normal, E-cores, Eco", RulesFormatter.RuleActionSummary(action));
    }

    [Fact]
    public void RuleActionSummary_PriorityOnly()
    {
        var action = new RuleAction { Priority = PriorityClass.PriorityHigh };
        Assert.Equal("High", RulesFormatter.RuleActionSummary(action));
    }

    [Fact]
    public void RuleActionSummary_CustomMaskRendersCompactly()
    {
        var action = new RuleAction
        {
            AffinityMode = CoreAffinityMode.CustomMask,
            AffinityMask = 0b1111UL,
        };
        Assert.Equal("cores 0-3", RulesFormatter.RuleActionSummary(action));
    }

    [Fact]
    public void RuleActionSummary_NoChange()
    {
        Assert.Equal("No change", RulesFormatter.RuleActionSummary(new RuleAction()));
        Assert.Equal("No change", RulesFormatter.RuleActionSummary(null));
    }

    [Fact]
    public void RuleSummary_ComposesImageActionAndTrigger()
    {
        var rule = new Rule
        {
            MatchImage = "chrome.exe",
            Trigger = RuleTrigger.WhileRunning,
            Action = new RuleAction
            {
                Priority = PriorityClass.PriorityBelowNormal,
                AffinityMode = CoreAffinityMode.PreferECores,
                EcoQos = true,
            },
        };
        Assert.Equal(
            "chrome.exe → Below Normal, E-cores, Eco — while running",
            RulesFormatter.RuleSummary(rule));
    }

    [Fact]
    public void RuleSummary_MissingImageIsObvious()
    {
        var rule = new Rule { Trigger = RuleTrigger.OnDcPower, Action = new RuleAction() };
        Assert.Equal("(no match) → No change — on battery", RulesFormatter.RuleSummary(rule));
    }

    [Fact]
    public void RuleSummary_NullIsEmpty()
    {
        Assert.Equal(string.Empty, RulesFormatter.RuleSummary(null));
    }

    [Theory]
    [InlineData(true, "Enabled")]
    [InlineData(false, "Disabled")]
    public void EnabledLabel_MapsFlag(bool enabled, string expected)
    {
        Assert.Equal(expected, RulesFormatter.EnabledLabel(enabled));
    }

    [Theory]
    [InlineData(true, "Active")]
    [InlineData(false, "Inactive")]
    public void ActiveLabel_MapsFlag(bool active, string expected)
    {
        Assert.Equal(expected, RulesFormatter.ActiveLabel(active));
    }

    // ---- Simulation transitions --------------------------------------------

    [Theory]
    [InlineData("Normal", "Below Normal", "Normal → Below Normal")]
    [InlineData("Normal", "Normal", "Normal")]
    [InlineData("Normal", "", "Normal")]
    [InlineData("Normal", null, "Normal")]
    [InlineData("", "High", "— → High")]
    [InlineData(null, null, "—")]
    public void TransitionText_CollapsesNonChanges(string? current, string? next, string expected)
    {
        Assert.Equal(expected, RulesFormatter.TransitionText(current, next));
    }

    [Theory]
    [InlineData(true, "Eco on")]
    [InlineData(false, "—")]
    public void EcoChangeText_MapsFlag(bool change, string expected)
    {
        Assert.Equal(expected, RulesFormatter.EcoChangeText(change));
    }

    [Fact]
    public void BlockedReasonText_PrefersServiceReason()
    {
        Assert.Equal("csrss.exe is protected.",
            RulesFormatter.BlockedReasonText("csrss.exe is protected."));
    }

    [Theory]
    [InlineData("")]
    [InlineData(null)]
    [InlineData("   ")]
    public void BlockedReasonText_FallsBackToStandardLine(string? reason)
    {
        Assert.Equal("System-critical — Atlas won't touch this.",
            RulesFormatter.BlockedReasonText(reason));
    }

    [Theory]
    [InlineData(0, 0, "No running processes match this rule right now.")]
    [InlineData(1, 0, "1 process would be affected.")]
    [InlineData(3, 0, "3 processes would be affected.")]
    [InlineData(3, 1, "2 processes would be affected, 1 protected target left untouched.")]
    [InlineData(4, 2, "2 processes would be affected, 2 protected targets left untouched.")]
    public void SimulationSummary_CountsAffectedAndProtected(int total, int blocked, string expected)
    {
        Assert.Equal(expected, RulesFormatter.SimulationSummary(total, blocked));
    }

    // ---- Interventions -----------------------------------------------------

    [Theory]
    [InlineData("Below Normal, E-cores", "Below Normal, E-cores")]
    [InlineData("", "Policy applied")]
    [InlineData(null, "Policy applied")]
    public void InterventionApplied_FallsBack(string? applied, string expected)
    {
        Assert.Equal(expected, RulesFormatter.InterventionApplied(applied));
    }

    [Theory]
    [InlineData(0, "just now")]        // sinceMs <= 0
    [InlineData(30_000, "just now")]   // under a minute
    [InlineData(60_000, "1m")]
    [InlineData(5 * 60_000, "5m")]
    [InlineData(60 * 60_000, "1h")]
    [InlineData(3 * 60 * 60_000L, "3h")]
    [InlineData(24 * 60 * 60_000L, "1d")]
    [InlineData(50 * 60 * 60_000L, "2d")]
    public void RelativeSince_FormatsDurations(long deltaMs, string expected)
    {
        const long now = 1_000_000_000_000;
        long since = deltaMs == 0 ? 0 : now - deltaMs;
        Assert.Equal(expected, RulesFormatter.RelativeSince(since, now));
    }

    [Fact]
    public void RelativeSince_FutureTimestampIsJustNow()
    {
        Assert.Equal("just now", RulesFormatter.RelativeSince(2_000, 1_000));
    }

    [Fact]
    public void InterventionLine_ComposesFullLine()
    {
        const long now = 1_000_000_000_000;
        var intervention = new Intervention
        {
            RuleId = 7,
            RuleName = "Gaming",
            Pid = 4242,
            ImageName = "chrome.exe",
            Applied = "Below Normal, E-cores",
            SinceMs = now - 3 * 60_000,
        };
        Assert.Equal(
            "chrome.exe (pid 4242) — Below Normal, E-cores · via Gaming · 3m",
            RulesFormatter.InterventionLine(intervention, now));
    }

    [Fact]
    public void InterventionLine_HandlesBlankNames()
    {
        const long now = 1_000_000_000_000;
        var intervention = new Intervention { Pid = 10, SinceMs = now };
        Assert.Equal(
            "(unknown) (pid 10) — Policy applied · via a rule · just now",
            RulesFormatter.InterventionLine(intervention, now));
    }

    [Fact]
    public void InterventionLine_NullIsEmpty()
    {
        Assert.Equal(string.Empty, RulesFormatter.InterventionLine(null, 0));
    }
}
