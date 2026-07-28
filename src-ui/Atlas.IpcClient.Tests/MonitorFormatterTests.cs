using Atlas.IpcClient;
using Atlas.V0;
using Xunit;

namespace Atlas.IpcClient.Tests;

public class MonitorFormatterTests
{
    // ---- Network: protocol / state -----------------------------------------

    [Theory]
    [InlineData(L4Protocol.Tcp, "TCP")]
    [InlineData(L4Protocol.Udp, "UDP")]
    [InlineData(L4Protocol.Unspecified, "—")]
    public void L4ProtocolLabel_MapsProtocols(L4Protocol protocol, string expected)
    {
        Assert.Equal(expected, MonitorFormatter.L4ProtocolLabel(protocol));
    }

    [Theory]
    [InlineData(TcpState.TcpEstablished, "Established")]
    [InlineData(TcpState.TcpListen, "Listening")]
    [InlineData(TcpState.TcpTimeWait, "Time wait")]
    [InlineData(TcpState.TcpCloseWait, "Close wait")]
    [InlineData(TcpState.Unspecified, "—")]
    public void TcpStateLabel_MapsStates(TcpState state, string expected)
    {
        Assert.Equal(expected, MonitorFormatter.TcpStateLabel(state));
    }

    [Theory]
    [InlineData(TcpState.TcpEstablished, "active")]
    [InlineData(TcpState.TcpListen, "listen")]
    [InlineData(TcpState.TcpTimeWait, "transitional")]
    [InlineData(TcpState.TcpSynSent, "transitional")]
    [InlineData(TcpState.TcpClosed, "idle")]
    [InlineData(TcpState.Unspecified, "none")]
    public void TcpStateToken_UsesCalmTokens(TcpState state, string expected)
    {
        Assert.Equal(expected, MonitorFormatter.TcpStateToken(state));
    }

    [Fact]
    public void TcpStateToken_NoStateIsAlarming()
    {
        // Tone contract: no TCP state maps to a danger/alarm token.
        foreach (TcpState s in System.Enum.GetValues(typeof(TcpState)))
        {
            var token = MonitorFormatter.TcpStateToken(s);
            Assert.DoesNotContain(token, new[] { "danger", "alarm", "error", "critical" });
        }
    }

    // ---- Network: endpoints ------------------------------------------------

    [Fact]
    public void EndpointText_Ipv4_JoinsAddrAndPort()
    {
        Assert.Equal("10.0.0.5:443", MonitorFormatter.EndpointText("10.0.0.5", 443, isIpv6: false));
    }

    [Fact]
    public void EndpointText_Ipv6_BracketsLiteral()
    {
        Assert.Equal("[fe80::1]:443", MonitorFormatter.EndpointText("fe80::1", 443, isIpv6: true));
    }

    [Fact]
    public void EndpointText_Ipv6_DoesNotDoubleBracket()
    {
        Assert.Equal("[::1]:80", MonitorFormatter.EndpointText("[::1]", 80, isIpv6: true));
    }

    [Fact]
    public void EndpointText_BlankAddr_BecomesWildcard()
    {
        Assert.Equal("*:8080", MonitorFormatter.EndpointText("", 8080, isIpv6: false));
    }

    [Fact]
    public void EndpointText_ZeroPort_ElidesPort()
    {
        Assert.Equal("10.0.0.5", MonitorFormatter.EndpointText("10.0.0.5", 0, isIpv6: false));
    }

    [Fact]
    public void DomainText_BlankBecomesDash()
    {
        Assert.Equal("—", MonitorFormatter.DomainText(""));
        Assert.Equal("example.com", MonitorFormatter.DomainText("example.com"));
    }

    [Fact]
    public void ProcessText_FallsBackToPid()
    {
        Assert.Equal("pid 1234", MonitorFormatter.ProcessText("", 1234));
        Assert.Equal("chrome.exe (1234)", MonitorFormatter.ProcessText("chrome.exe", 1234));
        Assert.Equal("—", MonitorFormatter.ProcessText("", 0));
    }

    // ---- Scheduled tasks ---------------------------------------------------

    [Fact]
    public void TaskEnabledLabel_DistinguishesStates()
    {
        Assert.Equal("Enabled", MonitorFormatter.TaskEnabledLabel(true));
        Assert.Equal("Disabled", MonitorFormatter.TaskEnabledLabel(false));
    }

    [Fact]
    public void TriggersText_BlankIsCalm()
    {
        Assert.Equal("No triggers", MonitorFormatter.TriggersText(" "));
        Assert.Equal("At logon", MonitorFormatter.TriggersText("At logon"));
    }

    [Theory]
    [InlineData(0, "Success")]
    [InlineData(0x00041303, "Never run")]
    [InlineData(0x00041301, "Running")]
    [InlineData(0x00041306, "Terminated")]
    [InlineData(unchecked((int)0x80070002), "0x80070002")]
    [InlineData(1, "0x00000001")]
    public void TaskLastResultText_MapsCodes(int code, string expected)
    {
        Assert.Equal(expected, MonitorFormatter.TaskLastResultText(code));
    }

    [Theory]
    [InlineData(0, "ok")]
    [InlineData(0x00041303, "idle")]
    [InlineData(0x00041301, "idle")]
    [InlineData(unchecked((int)0x80070002), "attention")]
    public void TaskLastResultToken_UsesCalmTokens(int code, string expected)
    {
        Assert.Equal(expected, MonitorFormatter.TaskLastResultToken(code));
    }

    [Fact]
    public void LastRunText_NeverWhenNonPositive()
    {
        Assert.Equal("Never", MonitorFormatter.LastRunText(0, nowMs: 10_000_000));
        Assert.Equal("Never", MonitorFormatter.LastRunText(-5, nowMs: 10_000_000));
    }

    [Fact]
    public void LastRunText_RelativePastWhenSet()
    {
        // 2 hours before "now".
        long now = 100_000_000;
        long twoHoursAgo = now - 2 * 60 * 60 * 1000;
        Assert.Equal("2h ago", MonitorFormatter.LastRunText(twoHoursAgo, now));
    }

    [Fact]
    public void NextRunText_NotScheduledWhenNonPositive()
    {
        Assert.Equal("Not scheduled", MonitorFormatter.NextRunText(0, nowMs: 10_000_000));
    }

    [Fact]
    public void NextRunText_RelativeFutureWhenSet()
    {
        long now = 100_000_000;
        long inThreeDays = now + 3L * 24 * 60 * 60 * 1000;
        Assert.Equal("in 3d", MonitorFormatter.NextRunText(inThreeDays, now));
    }

    [Theory]
    [InlineData(0, "just now")]
    [InlineData(30_000, "just now")]
    [InlineData(5 * 60_000L, "5m ago")]
    [InlineData(2 * 60 * 60_000L, "2h ago")]
    [InlineData(3 * 24 * 60 * 60_000L, "3d ago")]
    [InlineData(14 * 24 * 60 * 60_000L, "2w ago")]
    public void RelativePast_Buckets(long ageMs, string expected)
    {
        long now = 1_000_000_000_000;
        Assert.Equal(expected, MonitorFormatter.RelativePast(now - ageMs, now));
    }

    [Fact]
    public void RelativeFuture_DueNowWhenPast()
    {
        long now = 1_000_000_000_000;
        Assert.Equal("Due now", MonitorFormatter.RelativeFuture(now - 5, now));
    }

    [Fact]
    public void RunLevelText_DescribesPrivilege()
    {
        Assert.Equal("Highest privileges", MonitorFormatter.RunLevelText(true));
        Assert.Equal("Limited privileges", MonitorFormatter.RunLevelText(false));
    }

    // ---- Boot analysis -----------------------------------------------------

    [Theory]
    [InlineData(0u, "—")]
    [InlineData(8000u, "8s")]
    [InlineData(500u, "1s")]
    [InlineData(72000u, "1m 12s")]
    [InlineData(60000u, "1m 0s")]
    public void BootDurationText_Formats(uint ms, string expected)
    {
        Assert.Equal(expected, MonitorFormatter.BootDurationText(ms));
    }

    [Fact]
    public void BootDegraded_CalmWording()
    {
        Assert.Equal("Slower than usual", MonitorFormatter.BootDegradedText(true));
        Assert.Equal("Normal", MonitorFormatter.BootDegradedText(false));
        Assert.Equal("attention", MonitorFormatter.BootDegradedToken(true));
        Assert.Equal("ok", MonitorFormatter.BootDegradedToken(false));
    }

    [Fact]
    public void BootTimeText_NonPositiveIsDash()
    {
        Assert.Equal("—", MonitorFormatter.BootTimeText(0));
    }

    // ---- Battery -----------------------------------------------------------

    [Fact]
    public void BatteryPercentText_Formats()
    {
        Assert.Equal("87%", MonitorFormatter.BatteryPercentText(87));
    }

    [Fact]
    public void BatteryStateSummary_Discharging_WithRateAndRuntime()
    {
        // -12400 mW = 12.4 W discharge; 9600 s = 2h 40m.
        Assert.Equal(
            "Discharging 12.4 W, ~2h 40m left",
            MonitorFormatter.BatteryStateSummary(charging: false, onAc: false, rateMw: -12400, estRuntimeS: 9600));
    }

    [Fact]
    public void BatteryStateSummary_Charging_ShowsRate()
    {
        Assert.Equal(
            "Charging 30.1 W",
            MonitorFormatter.BatteryStateSummary(charging: true, onAc: true, rateMw: 30100, estRuntimeS: 0));
    }

    [Fact]
    public void BatteryStateSummary_OnAcIdle()
    {
        Assert.Equal(
            "On AC power",
            MonitorFormatter.BatteryStateSummary(charging: false, onAc: true, rateMw: 0, estRuntimeS: 0));
    }

    [Theory]
    [InlineData(0L, "—")]
    [InlineData(30L, "<1m")]
    [InlineData(45 * 60L, "45m")]
    [InlineData(9600L, "2h 40m")]
    public void RuntimeText_Buckets(long seconds, string expected)
    {
        Assert.Equal(expected, MonitorFormatter.RuntimeText(seconds));
    }

    [Fact]
    public void BatteryHealthText_NotReportedWhenZero()
    {
        Assert.Equal("Health not reported", MonitorFormatter.BatteryHealthText(0));
        Assert.Equal("Health 92% (of design)", MonitorFormatter.BatteryHealthText(92));
    }

    [Fact]
    public void CycleCountText_NotReportedWhenZero()
    {
        Assert.Equal("Cycle count not reported", MonitorFormatter.CycleCountText(0));
        Assert.Equal("1 cycle", MonitorFormatter.CycleCountText(1));
        Assert.Equal("412 cycles", MonitorFormatter.CycleCountText(412));
    }

    // ---- Thermal -----------------------------------------------------------

    [Fact]
    public void TemperatureText_OneDecimal()
    {
        Assert.Equal("42.5 °C", MonitorFormatter.TemperatureText(42.5));
    }

    [Fact]
    public void ThermalSourceText_BlankIsDash()
    {
        Assert.Equal("—", MonitorFormatter.ThermalSourceText(" "));
        Assert.Equal("ACPI thermal zone", MonitorFormatter.ThermalSourceText("ACPI thermal zone"));
    }

    [Theory]
    [InlineData(GpuTelemetrySource.GpuSourceWindowsWddm, "Windows WDDM")]
    [InlineData(GpuTelemetrySource.GpuSourceNvidiaNvml, "NVIDIA NVML")]
    [InlineData(GpuTelemetrySource.GpuSourceUnspecified, "source unavailable")]
    public void GpuSourceText_IsExplicit(GpuTelemetrySource source, string expected) =>
        Assert.Equal(expected, MonitorFormatter.GpuSourceText(source));

    [Theory]
    [InlineData(GpuAvailabilityReason.GpuAvailabilityProviderMissing, "provider_missing")]
    [InlineData(GpuAvailabilityReason.GpuAvailabilityHelperTimeout, "helper_timeout")]
    [InlineData(GpuAvailabilityReason.GpuAvailabilityHelperBackoff, "helper_backoff")]
    [InlineData(GpuAvailabilityReason.GpuAvailabilityUnsupportedMetric, "unsupported_metric")]
    [InlineData(GpuAvailabilityReason.GpuAvailabilityDeviceLost, "device_lost")]
    public void GpuAvailabilityCode_RemainsStable(GpuAvailabilityReason reason, string expected) =>
        Assert.Equal(expected, MonitorFormatter.GpuAvailabilityCode(reason));

    // ---- Shared unavailable helper -----------------------------------------

    [Fact]
    public void UnavailableReason_PrefersServiceReason()
    {
        Assert.Equal("no battery present",
            MonitorFormatter.UnavailableReason("no battery present", "fallback"));
        Assert.Equal("fallback",
            MonitorFormatter.UnavailableReason("", "fallback"));
    }
}
