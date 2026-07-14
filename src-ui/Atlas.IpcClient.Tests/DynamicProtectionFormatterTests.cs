using Atlas.IpcClient;
using Atlas.V0;
using Xunit;

namespace Atlas.IpcClient.Tests;

public class DynamicProtectionFormatterTests
{
    // ---- Permille <-> percent ---------------------------------------------

    [Theory]
    [InlineData(0u, 0.0)]
    [InlineData(800u, 80.0)]
    [InlineData(805u, 80.5)]
    [InlineData(1000u, 100.0)]
    public void PermilleToPercent_Converts(uint permille, double expected)
    {
        Assert.Equal(expected, DynamicProtectionFormatter.PermilleToPercent(permille), 3);
    }

    [Theory]
    [InlineData(80.0, 800u)]
    [InlineData(80.5, 805u)]
    [InlineData(10.0, 100u)]
    [InlineData(99.9, 999u)]
    public void PercentToPermille_Converts(double percent, uint expected)
    {
        Assert.Equal(expected, DynamicProtectionFormatter.PercentToPermille(percent));
    }

    [Fact]
    public void PercentToPermille_NeverNegative()
    {
        Assert.Equal(0u, DynamicProtectionFormatter.PercentToPermille(-5.0));
    }

    [Fact]
    public void PermilleRoundTrip_IsStable()
    {
        uint original = 805;
        double pct = DynamicProtectionFormatter.PermilleToPercent(original);
        Assert.Equal(original, DynamicProtectionFormatter.PercentToPermille(pct));
    }

    // ---- Threshold + percent text -----------------------------------------

    [Theory]
    [InlineData(800u, "80%")]
    [InlineData(805u, "80.5%")]
    [InlineData(100u, "10%")]
    [InlineData(1000u, "100%")]
    public void ThresholdPercentText_IsCompact(uint permille, string expected)
    {
        Assert.Equal(expected, DynamicProtectionFormatter.ThresholdPercentText(permille));
    }

    [Theory]
    [InlineData(80.0, "80%")]
    [InlineData(80.5, "80.5%")]
    public void PercentText_TrimsTrailingZero(double percent, string expected)
    {
        Assert.Equal(expected, DynamicProtectionFormatter.PercentText(percent));
    }

    // ---- Duration text -----------------------------------------------------

    [Theory]
    [InlineData(0u, "0s")]
    [InlineData(30u, "30s")]
    [InlineData(59u, "59s")]
    [InlineData(60u, "1m")]
    [InlineData(300u, "5m")]
    [InlineData(90u, "1m 30s")]
    [InlineData(3600u, "1h")]
    [InlineData(3660u, "1h 1m")]
    public void DurationText_IsCompact(uint seconds, string expected)
    {
        Assert.Equal(expected, DynamicProtectionFormatter.DurationText(seconds));
    }

    // ---- Clamping ----------------------------------------------------------

    [Theory]
    [InlineData(5.0, DynamicProtectionFormatter.MinThresholdPercent)]
    [InlineData(150.0, DynamicProtectionFormatter.MaxThresholdPercent)]
    [InlineData(80.0, 80.0)]
    public void ClampThresholdPercent_StaysInRange(double input, double expected)
    {
        Assert.Equal(expected, DynamicProtectionFormatter.ClampThresholdPercent(input), 3);
    }

    [Theory]
    [InlineData(1u, DynamicProtectionFormatter.MinSustainSeconds)]
    [InlineData(99999u, DynamicProtectionFormatter.MaxSustainSeconds)]
    [InlineData(30u, 30u)]
    public void ClampSustainSeconds_StaysInRange(uint input, uint expected)
    {
        Assert.Equal(expected, DynamicProtectionFormatter.ClampSustainSeconds(input));
    }

    [Theory]
    [InlineData(1u, DynamicProtectionFormatter.MinMaxInterventionSeconds)]
    [InlineData(99999u, DynamicProtectionFormatter.MaxMaxInterventionSeconds)]
    [InlineData(300u, 300u)]
    public void ClampMaxInterventionSeconds_StaysInRange(uint input, uint expected)
    {
        Assert.Equal(expected, DynamicProtectionFormatter.ClampMaxInterventionSeconds(input));
    }

    // ---- Config summary: calm, honest, off-by-default ---------------------

    [Fact]
    public void ConfigSummary_Null_ReadsAsOff()
    {
        Assert.Equal("Off — Atlas isn't easing back any app.",
            DynamicProtectionFormatter.ConfigSummary(null));
    }

    [Fact]
    public void ConfigSummary_Disabled_ReadsAsOff()
    {
        var cfg = new DynamicProtectionConfig
        {
            Enabled = false,
            CpuThresholdPermille = 800,
            SustainSeconds = 30,
            MaxInterventionSeconds = 300,
        };
        Assert.Equal("Off — Atlas isn't easing back any app.",
            DynamicProtectionFormatter.ConfigSummary(cfg));
    }

    [Fact]
    public void ConfigSummary_Enabled_StatesWhatItDoes()
    {
        var cfg = new DynamicProtectionConfig
        {
            Enabled = true,
            CpuThresholdPermille = 800,
            SustainSeconds = 30,
            MaxInterventionSeconds = 300,
        };
        var summary = DynamicProtectionFormatter.ConfigSummary(cfg);
        Assert.Equal(
            "On — eases back a background app that stays above 80% CPU for 30s, and restores it within 5m.",
            summary);
    }

    [Fact]
    public void ConfigSummary_NeverAlarmist()
    {
        var cfg = new DynamicProtectionConfig
        {
            Enabled = true,
            CpuThresholdPermille = 950,
            SustainSeconds = 10,
            MaxInterventionSeconds = 60,
        };
        var summary = DynamicProtectionFormatter.ConfigSummary(cfg).ToLowerInvariant();
        // A temporary, reversible dampening is not a threat: no alarm words.
        Assert.DoesNotContain("kill", summary);
        Assert.DoesNotContain("terminate", summary);
        Assert.DoesNotContain("danger", summary);
        // ...and the reversible promise stays present.
        Assert.Contains("restores it", summary);
    }
}
