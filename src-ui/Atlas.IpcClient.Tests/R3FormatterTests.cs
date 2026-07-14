using Atlas.IpcClient;
using Atlas.V0;
using Xunit;

namespace Atlas.IpcClient.Tests;

public class R3FormatterTests
{
    // ---- System-change kind labels ----------------------------------------

    [Theory]
    [InlineData(SystemChangeKind.AppInstalled, "App installed")]
    [InlineData(SystemChangeKind.AppUpdated, "App updated")]
    [InlineData(SystemChangeKind.AppRemoved, "App removed")]
    [InlineData(SystemChangeKind.DriverInstalled, "Driver installed")]
    [InlineData(SystemChangeKind.DriverUpdated, "Driver updated")]
    [InlineData(SystemChangeKind.WindowsUpdate, "Windows update")]
    [InlineData(SystemChangeKind.ServiceInstalled, "Service installed")]
    [InlineData(SystemChangeKind.ServiceConfigChanged, "Service changed")]
    [InlineData(SystemChangeKind.ServiceRemoved, "Service removed")]
    [InlineData(SystemChangeKind.StartupAdded, "Startup item added")]
    [InlineData(SystemChangeKind.StartupRemoved, "Startup item removed")]
    [InlineData(SystemChangeKind.ScheduledTaskAdded, "Scheduled task added")]
    [InlineData(SystemChangeKind.ScheduledTaskRemoved, "Scheduled task removed")]
    [InlineData(SystemChangeKind.PowerPlanChanged, "Power plan changed")]
    [InlineData(SystemChangeKind.DefaultAppChanged, "Default app changed")]
    [InlineData(SystemChangeKind.Unspecified, "System change")]
    public void SystemChangeKindLabel_MapsKinds(SystemChangeKind kind, string expected)
    {
        Assert.Equal(expected, R3Formatter.SystemChangeKindLabel(kind));
    }

    [Fact]
    public void SystemChangeKindGlyph_IsNonEmptyForEveryDeclaredKind()
    {
        foreach (SystemChangeKind kind in System.Enum.GetValues(typeof(SystemChangeKind)))
        {
            Assert.False(string.IsNullOrEmpty(R3Formatter.SystemChangeKindGlyph(kind)));
        }
    }

    // ---- Calm category tokens: NEVER the danger palette --------------------

    [Theory]
    [InlineData(SystemChangeKind.AppInstalled, "install")]
    [InlineData(SystemChangeKind.AppUpdated, "update")]
    [InlineData(SystemChangeKind.WindowsUpdate, "update")]
    [InlineData(SystemChangeKind.AppRemoved, "remove")]
    [InlineData(SystemChangeKind.ServiceRemoved, "remove")]
    [InlineData(SystemChangeKind.StartupRemoved, "remove")]
    [InlineData(SystemChangeKind.ScheduledTaskRemoved, "remove")]
    [InlineData(SystemChangeKind.DriverInstalled, "driver")]
    [InlineData(SystemChangeKind.DriverUpdated, "driver")]
    [InlineData(SystemChangeKind.ServiceInstalled, "service")]
    [InlineData(SystemChangeKind.ServiceConfigChanged, "service")]
    [InlineData(SystemChangeKind.StartupAdded, "startup")]
    [InlineData(SystemChangeKind.ScheduledTaskAdded, "task")]
    [InlineData(SystemChangeKind.PowerPlanChanged, "power")]
    [InlineData(SystemChangeKind.DefaultAppChanged, "default")]
    [InlineData(SystemChangeKind.Unspecified, "default")]
    public void SystemChangeCategoryToken_MapsKinds(SystemChangeKind kind, string expected)
    {
        Assert.Equal(expected, R3Formatter.SystemChangeCategoryToken(kind));
    }

    [Fact]
    public void SystemChangeCategoryToken_NeverAlarmist()
    {
        // A change is information, not a threat: no token may be a danger word that
        // a converter could map to the red critical palette.
        foreach (SystemChangeKind kind in System.Enum.GetValues(typeof(SystemChangeKind)))
        {
            var token = R3Formatter.SystemChangeCategoryToken(kind);
            Assert.DoesNotContain(token, new[] { "critical", "danger", "error", "alert" });
        }
    }

    // ---- Change summary + provenance --------------------------------------

    [Fact]
    public void ChangeSummary_CombinesKindAndSubject()
    {
        Assert.Equal("App installed: Zoom",
            R3Formatter.ChangeSummary(SystemChangeKind.AppInstalled, "Zoom"));
    }

    [Fact]
    public void ChangeSummary_FallsBackToKindLabel_WhenNoSubject()
    {
        Assert.Equal("Windows update",
            R3Formatter.ChangeSummary(SystemChangeKind.WindowsUpdate, "   "));
        Assert.Equal("App updated",
            R3Formatter.ChangeSummary(SystemChangeKind.AppUpdated, null));
    }

    [Fact]
    public void ChangeProvenance_JoinsPublisherAndResponsible()
    {
        Assert.Equal("Zoom Video Communications • via msiexec.exe",
            R3Formatter.ChangeProvenance("Zoom Video Communications", "msiexec.exe"));
    }

    [Fact]
    public void ChangeProvenance_HandlesEitherPartMissing()
    {
        Assert.Equal("Contoso", R3Formatter.ChangeProvenance("Contoso", ""));
        Assert.Equal("via setup.exe", R3Formatter.ChangeProvenance(null, "setup.exe"));
        Assert.Equal(string.Empty, R3Formatter.ChangeProvenance("  ", null));
    }

    [Theory]
    [InlineData(true, "Reversible")]
    [InlineData(false, "")]
    public void ReversibleLabel_MapsFlag(bool reversible, string expected)
    {
        Assert.Equal(expected, R3Formatter.ReversibleLabel(reversible));
    }

    // ---- Crash kind labels -------------------------------------------------

    [Theory]
    [InlineData(CrashKind.AppCrash, "App crash")]
    [InlineData(CrashKind.AppHang, "App stopped responding")]
    [InlineData(CrashKind.Bugcheck, "System bugcheck")]
    [InlineData(CrashKind.ServiceFailure, "Service failure")]
    [InlineData(CrashKind.UnexpectedShutdown, "Unexpected shutdown")]
    [InlineData(CrashKind.Unspecified, "Reliability event")]
    public void CrashKindLabel_MapsKinds(CrashKind kind, string expected)
    {
        Assert.Equal(expected, R3Formatter.CrashKindLabel(kind));
    }

    [Fact]
    public void CrashKindGlyph_IsNonEmptyForEveryDeclaredKind()
    {
        foreach (CrashKind kind in System.Enum.GetValues(typeof(CrashKind)))
        {
            Assert.False(string.IsNullOrEmpty(R3Formatter.CrashKindGlyph(kind)));
        }
    }

    // ---- Crash caution tokens: caution, NEVER danger ----------------------

    [Theory]
    [InlineData(CrashKind.AppCrash, "caution")]
    [InlineData(CrashKind.AppHang, "caution")]
    [InlineData(CrashKind.Bugcheck, "caution")]
    [InlineData(CrashKind.ServiceFailure, "caution")]
    [InlineData(CrashKind.UnexpectedShutdown, "neutral")]
    [InlineData(CrashKind.Unspecified, "neutral")]
    public void CrashCautionToken_MapsKinds(CrashKind kind, string expected)
    {
        Assert.Equal(expected, R3Formatter.CrashCautionToken(kind));
    }

    [Fact]
    public void CrashCautionToken_NeverReachesCritical()
    {
        // A crash record is history + context, not an alarm: no kind is painted the
        // red critical/danger palette.
        foreach (CrashKind kind in System.Enum.GetValues(typeof(CrashKind)))
        {
            var token = R3Formatter.CrashCautionToken(kind);
            Assert.DoesNotContain(token, new[] { "critical", "danger", "error" });
        }
    }

    // ---- Crash subject / fault / context ----------------------------------

    [Fact]
    public void CrashSubjectText_FallsBackToKindLabel()
    {
        Assert.Equal("chrome.exe", R3Formatter.CrashSubjectText(CrashKind.AppCrash, "chrome.exe"));
        Assert.Equal("System bugcheck", R3Formatter.CrashSubjectText(CrashKind.Bugcheck, "  "));
    }

    [Fact]
    public void CrashFaultLine_JoinsFaultAndException()
    {
        Assert.Equal("ntdll.dll • 0xC0000005",
            R3Formatter.CrashFaultLine("ntdll.dll", "0xC0000005"));
    }

    [Fact]
    public void CrashFaultLine_HandlesEitherPartMissing()
    {
        Assert.Equal("ntdll.dll", R3Formatter.CrashFaultLine("ntdll.dll", ""));
        Assert.Equal("0x1A", R3Formatter.CrashFaultLine(null, "0x1A"));
        Assert.Equal(string.Empty, R3Formatter.CrashFaultLine(" ", null));
    }

    [Fact]
    public void CrashContextLine_TrimsAndFallsBack()
    {
        Assert.Equal("Peak memory 92% shortly before",
            R3Formatter.CrashContextLine("  Peak memory 92% shortly before  "));
        Assert.Equal("(context noted)", R3Formatter.CrashContextLine("   "));
        Assert.Equal("(context noted)", R3Formatter.CrashContextLine(null));
    }

    [Fact]
    public void ContextIntro_IsHedged_NotACause()
    {
        // The framing must stay correlation-not-blame.
        Assert.Contains("not a cause", R3Formatter.ContextIntro);
    }
}
