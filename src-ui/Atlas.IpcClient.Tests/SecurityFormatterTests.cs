using System;
using Atlas.IpcClient;
using Xunit;

namespace Atlas.IpcClient.Tests;

public class SecurityFormatterTests
{
    private const string Sha =
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    // ---- SHA-256 grouping / short form -------------------------------------

    [Fact]
    public void Sha256Grouped_GroupsIntoEightCharBlocks()
    {
        Assert.Equal(
            "e3b0c442 98fc1c14 9afbf4c8 996fb924 27ae41e4 649b934c a495991b 7852b855",
            SecurityFormatter.Sha256Grouped(Sha));
    }

    [Theory]
    [InlineData(null)]
    [InlineData("")]
    [InlineData("   ")]
    public void Sha256Grouped_BlankIsDash(string? input)
    {
        Assert.Equal("—", SecurityFormatter.Sha256Grouped(input));
    }

    [Fact]
    public void Sha256Grouped_ToleratesPrefixAndWhitespace()
    {
        Assert.Equal(
            "e3b0c442 98fc1c14 9afbf4c8 996fb924 27ae41e4 649b934c a495991b 7852b855",
            SecurityFormatter.Sha256Grouped("sha256: e3b0c442 98fc1c14 9afbf4c8 996fb924 27ae41e4 649b934c a495991b 7852b855"));
    }

    [Fact]
    public void Sha256Short_IsFirstAndLastEight()
    {
        Assert.Equal("e3b0c442…7852b855", SecurityFormatter.Sha256Short(Sha));
    }

    [Fact]
    public void Sha256Short_ShortDigestReturnedWhole()
    {
        Assert.Equal("abcd1234", SecurityFormatter.Sha256Short("abcd1234"));
    }

    [Fact]
    public void Sha256Raw_StripsPrefixAndWhitespace()
    {
        Assert.Equal(Sha, SecurityFormatter.Sha256Raw("sha256:e3b0c442 98fc1c14 9afbf4c8 996fb924 27ae41e4 649b934c a495991b 7852b855"));
    }

    [Fact]
    public void Sha256Raw_BlankIsEmpty()
    {
        Assert.Equal(string.Empty, SecurityFormatter.Sha256Raw(null));
    }

    // ---- Thumbprint grouping -----------------------------------------------

    [Fact]
    public void ThumbprintGrouped_UppercasesAndGroupsByFour()
    {
        Assert.Equal(
            "A1B2 C3D4 E5F6 0718 2930",
            SecurityFormatter.ThumbprintGrouped("a1b2c3d4e5f6071829 30"));
    }

    [Fact]
    public void ThumbprintGrouped_BlankIsDash()
    {
        Assert.Equal("—", SecurityFormatter.ThumbprintGrouped(""));
    }

    // ---- Certificate validity ----------------------------------------------

    [Fact]
    public void CertValidUntil_FormatsExpiryDay()
    {
        // 2027-01-02 12:00:00 UTC — assert on the local-date rendering.
        long ms = new DateTimeOffset(2027, 1, 2, 12, 0, 0, TimeSpan.Zero).ToUnixTimeMilliseconds();
        var expected = "valid until " +
            DateTimeOffset.FromUnixTimeMilliseconds(ms).LocalDateTime.ToString("yyyy-MM-dd");
        Assert.Equal(expected, SecurityFormatter.CertValidUntil(ms));
    }

    [Fact]
    public void CertValidUntil_NonPositiveIsDash()
    {
        Assert.Equal("—", SecurityFormatter.CertValidUntil(0));
    }

    [Fact]
    public void CertValidityNote_NormalWindowIsEmpty()
    {
        long now = new DateTimeOffset(2026, 7, 15, 0, 0, 0, TimeSpan.Zero).ToUnixTimeMilliseconds();
        long notBefore = new DateTimeOffset(2025, 1, 1, 0, 0, 0, TimeSpan.Zero).ToUnixTimeMilliseconds();
        long notAfter = new DateTimeOffset(2027, 1, 1, 0, 0, 0, TimeSpan.Zero).ToUnixTimeMilliseconds();
        Assert.Equal(string.Empty, SecurityFormatter.CertValidityNote(notBefore, notAfter, now));
        Assert.Equal("ok", SecurityFormatter.CertValidityToken(notBefore, notAfter, now));
    }

    [Fact]
    public void CertValidityNote_ExpiredIsFlaggedCalmly()
    {
        long now = new DateTimeOffset(2026, 7, 15, 0, 0, 0, TimeSpan.Zero).ToUnixTimeMilliseconds();
        long notBefore = new DateTimeOffset(2018, 1, 1, 0, 0, 0, TimeSpan.Zero).ToUnixTimeMilliseconds();
        long notAfter = new DateTimeOffset(2020, 1, 1, 0, 0, 0, TimeSpan.Zero).ToUnixTimeMilliseconds();
        Assert.StartsWith("expired ", SecurityFormatter.CertValidityNote(notBefore, notAfter, now));
        Assert.Equal("expired", SecurityFormatter.CertValidityToken(notBefore, notAfter, now));
    }

    [Fact]
    public void CertValidityNote_NearExpiryWithinWindow()
    {
        long now = new DateTimeOffset(2026, 7, 15, 0, 0, 0, TimeSpan.Zero).ToUnixTimeMilliseconds();
        long notBefore = new DateTimeOffset(2025, 1, 1, 0, 0, 0, TimeSpan.Zero).ToUnixTimeMilliseconds();
        long notAfter = new DateTimeOffset(2026, 7, 25, 0, 0, 0, TimeSpan.Zero).ToUnixTimeMilliseconds();
        Assert.StartsWith("expires soon", SecurityFormatter.CertValidityNote(notBefore, notAfter, now));
        Assert.Equal("caution", SecurityFormatter.CertValidityToken(notBefore, notAfter, now));
    }

    [Fact]
    public void CertValidityNote_NotYetValid()
    {
        long now = new DateTimeOffset(2026, 7, 15, 0, 0, 0, TimeSpan.Zero).ToUnixTimeMilliseconds();
        long notBefore = new DateTimeOffset(2026, 12, 1, 0, 0, 0, TimeSpan.Zero).ToUnixTimeMilliseconds();
        long notAfter = new DateTimeOffset(2028, 1, 1, 0, 0, 0, TimeSpan.Zero).ToUnixTimeMilliseconds();
        Assert.Equal("not yet valid", SecurityFormatter.CertValidityNote(notBefore, notAfter, now));
    }

    [Fact]
    public void CertNameText_BlankPlaceholder()
    {
        Assert.Equal("(unknown)", SecurityFormatter.CertNameText("  "));
        Assert.Equal("CN=Contoso", SecurityFormatter.CertNameText(" CN=Contoso "));
    }

    // ---- Privileges: name kept, gloss added, state neutral -----------------

    [Theory]
    [InlineData("SeDebugPrivilege", "Debug programs")]
    [InlineData("SeShutdownPrivilege", "Shut down the system")]
    [InlineData("SeBackupPrivilege", "Back up files and directories")]
    [InlineData("SeImpersonatePrivilege", "Impersonate a client after authentication")]
    [InlineData("SeChangeNotifyPrivilege", "Bypass traverse checking")]
    public void PrivilegeGloss_KnownNamesGetFriendlyGloss(string name, string gloss)
    {
        Assert.Equal(gloss, SecurityFormatter.PrivilegeGloss(name));
    }

    [Theory]
    [InlineData("SeMadeUpPrivilege")]
    [InlineData("")]
    [InlineData(null)]
    public void PrivilegeGloss_UnknownIsEmpty(string? name)
    {
        Assert.Equal(string.Empty, SecurityFormatter.PrivilegeGloss(name));
    }

    [Fact]
    public void PrivilegeState_IsNeutralInformational_NeverAlarmist()
    {
        Assert.Equal("enabled", SecurityFormatter.PrivilegeStateToken(true));
        Assert.Equal("available", SecurityFormatter.PrivilegeStateToken(false));
        Assert.Equal("Enabled", SecurityFormatter.PrivilegeStateLabel(true));
        Assert.Equal("Available", SecurityFormatter.PrivilegeStateLabel(false));

        // Neither state may be a danger word a converter could map to red.
        foreach (var enabled in new[] { true, false })
        {
            var token = SecurityFormatter.PrivilegeStateToken(enabled);
            Assert.DoesNotContain(token, new[] { "danger", "critical", "threat", "alert" });
        }
    }

    [Fact]
    public void PrivilegeNameText_BlankPlaceholder()
    {
        Assert.Equal("(unnamed privilege)", SecurityFormatter.PrivilegeNameText(""));
        Assert.Equal("SeDebugPrivilege", SecurityFormatter.PrivilegeNameText(" SeDebugPrivilege "));
    }

    // ---- Sandbox / groups / capabilities / mitigations ---------------------

    [Fact]
    public void AppContainerLabel_StatesFactPlainly()
    {
        Assert.Equal("App container (sandboxed)", SecurityFormatter.AppContainerLabel(true));
        Assert.Equal("Not app-contained", SecurityFormatter.AppContainerLabel(false));
    }

    [Fact]
    public void GroupAndCapabilityText_BlankPlaceholders()
    {
        Assert.Equal("(unnamed group)", SecurityFormatter.GroupText(null));
        Assert.Equal("BUILTIN\\Users", SecurityFormatter.GroupText("BUILTIN\\Users"));
        Assert.Equal("(unnamed capability)", SecurityFormatter.CapabilityText(""));
        Assert.Equal("internetClient", SecurityFormatter.CapabilityText(" internetClient "));
    }

    [Fact]
    public void MitigationLabel_PassesThroughTrimmed()
    {
        Assert.Equal("ASLR (high-entropy)", SecurityFormatter.MitigationLabel(" ASLR (high-entropy) "));
        Assert.Equal("—", SecurityFormatter.MitigationLabel("  "));
    }

    // ---- Coverage note (honesty) -------------------------------------------

    [Fact]
    public void LimitedCoverageNote_OnlyWhenLimited()
    {
        Assert.Equal(string.Empty, SecurityFormatter.LimitedCoverageNote(false));
        Assert.Equal(SecurityFormatter.LimitedCoverageMessage, SecurityFormatter.LimitedCoverageNote(true));
        // The note frames the gap as an elevation limitation, not suspicion.
        Assert.Contains("elevated Atlas", SecurityFormatter.LimitedCoverageMessage);
    }
}
