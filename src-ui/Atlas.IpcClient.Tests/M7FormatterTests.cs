using Atlas.IpcClient;
using Atlas.V0;
using Xunit;

namespace Atlas.IpcClient.Tests;

public class M7FormatterTests
{
    // ---- Privacy -----------------------------------------------------------

    [Theory]
    [InlineData(CapabilityKind.Camera, "Camera")]
    [InlineData(CapabilityKind.Microphone, "Microphone")]
    [InlineData(CapabilityKind.Location, "Location")]
    [InlineData(CapabilityKind.Unspecified, "Other")]
    public void CapabilityLabel_MapsKinds(CapabilityKind kind, string expected)
    {
        Assert.Equal(expected, M7Formatter.CapabilityLabel(kind));
    }

    [Fact]
    public void CapabilityGlyph_IsNonEmptyPerKind()
    {
        Assert.False(string.IsNullOrEmpty(M7Formatter.CapabilityGlyph(CapabilityKind.Camera)));
        Assert.False(string.IsNullOrEmpty(M7Formatter.CapabilityGlyph(CapabilityKind.Microphone)));
        Assert.False(string.IsNullOrEmpty(M7Formatter.CapabilityGlyph(CapabilityKind.Location)));
    }

    [Fact]
    public void PackagedLabel_DistinguishesPackagedAndDesktop()
    {
        Assert.Equal("Packaged", M7Formatter.PackagedLabel(true));
        Assert.Equal("Desktop app", M7Formatter.PackagedLabel(false));
    }

    [Fact]
    public void UsageStatus_InUse_SaysInUseNow()
    {
        Assert.Equal("In use now", M7Formatter.UsageStatus(true, 0, 0, nowMs: 10_000_000));
    }

    [Fact]
    public void UsageStatus_NotInUse_UsesStopTimeRelative()
    {
        // Stopped 2 hours ago.
        long now = 10_000_000_000;
        long stop = now - 2 * 60 * 60 * 1000;
        Assert.Equal("Last used 2h ago", M7Formatter.UsageStatus(false, now - 3 * 60 * 60 * 1000, stop, now));
    }

    [Fact]
    public void UsageStatus_NotInUse_FallsBackToStartWhenNoStop()
    {
        long now = 10_000_000_000;
        long start = now - 5 * 60 * 1000; // 5 minutes ago
        Assert.Equal("Last used 5m ago", M7Formatter.UsageStatus(false, start, 0, now));
    }

    [Fact]
    public void UsageStatus_NoTimestamps_SaysNotUsedRecently()
    {
        Assert.Equal("Not used recently", M7Formatter.UsageStatus(false, 0, 0, nowMs: 1000));
    }

    [Theory]
    [InlineData(0, "just now")]        // same instant
    [InlineData(30_000, "just now")]   // 30s
    [InlineData(5 * 60_000, "5m ago")]
    [InlineData(90 * 60_000, "1h ago")]
    [InlineData(3 * 24 * 60 * 60_000L, "3d ago")]
    public void RelativeTime_Bucketing(long agoMs, string expected)
    {
        long now = 100_000_000_000;
        Assert.Equal(expected, M7Formatter.RelativeTime(now - agoMs, now));
    }

    [Fact]
    public void RelativeTime_FutureIsJustNow()
    {
        Assert.Equal("just now", M7Formatter.RelativeTime(2000, 1000));
    }

    [Fact]
    public void PrivacyEventLine_UsesDisplayNameAndVerb()
    {
        var e = new PrivacyEvent
        {
            Capability = CapabilityKind.Camera,
            DisplayName = "Zoom",
            Started = true,
        };
        Assert.Equal("Camera • Zoom started", M7Formatter.PrivacyEventLine(e));
    }

    [Fact]
    public void PrivacyEventLine_StopFallsBackToAppId()
    {
        var e = new PrivacyEvent
        {
            Capability = CapabilityKind.Microphone,
            AppId = "SomeApp",
            Started = false,
        };
        Assert.Equal("Microphone • SomeApp stopped", M7Formatter.PrivacyEventLine(e));
    }

    [Fact]
    public void PrivacyEventLine_NoNamesFallsBackToUnknown()
    {
        var e = new PrivacyEvent { Capability = CapabilityKind.Location, Started = true };
        Assert.Contains("(unknown)", M7Formatter.PrivacyEventLine(e));
    }

    // ---- Startup -----------------------------------------------------------

    [Theory]
    [InlineData(StartupSource.RunKeyMachine, "Run keys")]
    [InlineData(StartupSource.RunKeyUser, "Run keys")]
    [InlineData(StartupSource.StartupFolderMachine, "Startup folders")]
    [InlineData(StartupSource.StartupFolderUser, "Startup folders")]
    [InlineData(StartupSource.ScheduledTask, "Tasks")]
    [InlineData(StartupSource.Service, "Services")]
    [InlineData(StartupSource.PackagedTask, "Packaged")]
    [InlineData(StartupSource.Unspecified, "Other")]
    public void StartupCategory_MapsSources(StartupSource source, string expected)
    {
        Assert.Equal(expected, M7Formatter.StartupCategory(source));
    }

    [Fact]
    public void StartupSourceLabel_DistinguishesScope()
    {
        Assert.Contains("machine", M7Formatter.StartupSourceLabel(StartupSource.RunKeyMachine));
        Assert.Contains("user", M7Formatter.StartupSourceLabel(StartupSource.RunKeyUser));
    }

    [Fact]
    public void EnabledLabel_Pills()
    {
        Assert.Equal("Enabled", M7Formatter.EnabledLabel(true));
        Assert.Equal("Disabled", M7Formatter.EnabledLabel(false));
    }

    // ---- Services ----------------------------------------------------------

    [Theory]
    [InlineData(ServiceState.ServiceRunning, "Running")]
    [InlineData(ServiceState.ServiceStopped, "Stopped")]
    [InlineData(ServiceState.ServicePaused, "Paused")]
    [InlineData(ServiceState.ServiceStartPending, "Starting")]
    public void ServiceStateLabel_MapsStates(ServiceState state, string expected)
    {
        Assert.Equal(expected, M7Formatter.ServiceStateLabel(state));
    }

    [Theory]
    [InlineData(ServiceState.ServiceRunning, "running")]
    [InlineData(ServiceState.ServiceStopped, "stopped")]
    [InlineData(ServiceState.ServiceStartPending, "transitional")]
    [InlineData(ServiceState.ServiceStopPending, "transitional")]
    [InlineData(ServiceState.ServicePaused, "transitional")]
    [InlineData(ServiceState.Unspecified, "unknown")]
    public void ServiceStateSeverity_MapsToTokens(ServiceState state, string expected)
    {
        Assert.Equal(expected, M7Formatter.ServiceStateSeverity(state));
    }

    [Theory]
    [InlineData(ServiceStartType.StartAuto, "Automatic")]
    [InlineData(ServiceStartType.StartManual, "Manual")]
    [InlineData(ServiceStartType.StartDisabled, "Disabled")]
    [InlineData(ServiceStartType.StartBoot, "Boot")]
    public void ServiceStartTypeLabel_MapsTypes(ServiceStartType startType, string expected)
    {
        Assert.Equal(expected, M7Formatter.ServiceStartTypeLabel(startType));
    }

    [Fact]
    public void ServiceStartTypeLabel_DelayedAutoStart_Annotated()
    {
        Assert.Equal("Automatic (delayed)",
            M7Formatter.ServiceStartTypeLabel(ServiceStartType.StartAuto, delayedAutoStart: true));
        // Delayed flag only applies to Automatic.
        Assert.Equal("Manual",
            M7Formatter.ServiceStartTypeLabel(ServiceStartType.StartManual, delayedAutoStart: true));
    }

    [Fact]
    public void PidText_BlankWhenZero()
    {
        Assert.Equal(string.Empty, M7Formatter.PidText(0));
        Assert.Equal("1234", M7Formatter.PidText(1234));
    }

    [Fact]
    public void Truncate_LongValueGetsEllipsis()
    {
        var s = new string('x', 200);
        var t = M7Formatter.Truncate(s, 80);
        Assert.Equal(80, t.Length);
        Assert.EndsWith("…", t);
    }

    [Fact]
    public void Truncate_ShortValueUnchanged()
    {
        Assert.Equal("short", M7Formatter.Truncate("short", 80));
    }

    [Fact]
    public void Truncate_NullIsEmpty()
    {
        Assert.Equal(string.Empty, M7Formatter.Truncate(null));
    }
}
