using Atlas.IpcClient;
using Xunit;

namespace Atlas.IpcClient.Tests;

public class R2FormatterTests
{
    // ---- Integrity / elevation / architecture ------------------------------

    [Theory]
    [InlineData("System", "System")]
    [InlineData("High", "High")]
    [InlineData("Medium", "Medium")]
    [InlineData("Low", "Low")]
    [InlineData("AppContainer", "AppContainer")]
    [InlineData("", "Unknown")]
    [InlineData(null, "Unknown")]
    public void IntegrityLabel_MapsLevels(string? level, string expected)
    {
        Assert.Equal(expected, R2Formatter.IntegrityLabel(level));
    }

    [Theory]
    [InlineData(true, "Elevated")]
    [InlineData(false, "Not elevated")]
    public void ElevationLabel_MapsFlag(bool elevated, string expected)
    {
        Assert.Equal(expected, R2Formatter.ElevationLabel(elevated));
    }

    [Theory]
    [InlineData("x64", "x64")]
    [InlineData("Arm64", "Arm64")]
    [InlineData("", "Unknown")]
    [InlineData(null, "Unknown")]
    public void ArchitectureLabel_MapsArch(string? arch, string expected)
    {
        Assert.Equal(expected, R2Formatter.ArchitectureLabel(arch));
    }

    // ---- Signature (trust tokens must stay calm) ---------------------------

    [Theory]
    [InlineData("Signed (Microsoft)", "Signed (Microsoft)")]
    [InlineData("Unsigned", "Unsigned")]
    [InlineData("", "Unknown")]
    [InlineData(null, "Unknown")]
    public void SignatureStatusLabel_NormalizesBlank(string? status, string expected)
    {
        Assert.Equal(expected, R2Formatter.SignatureStatusLabel(status));
    }

    [Theory]
    [InlineData("Signed (Microsoft)", "trusted")]
    [InlineData("Signed", "signed")]
    [InlineData("Unsigned", "caution")]
    [InlineData("Unknown", "unknown")]
    [InlineData("", "unknown")]
    [InlineData(null, "unknown")]
    public void SignatureTrustToken_MapsStatuses(string? status, string expected)
    {
        Assert.Equal(expected, R2Formatter.SignatureTrustToken(status));
    }

    [Fact]
    public void SignatureTrustToken_UnsignedIsCaution_NotADangerToken()
    {
        // An unsigned binary is common and legitimate — informational, not alarm.
        var token = R2Formatter.SignatureTrustToken("Unsigned");
        Assert.Equal("caution", token);
        Assert.NotEqual("critical", token);
        Assert.NotEqual("danger", token);
    }

    // ---- Publisher / package / user ----------------------------------------

    [Theory]
    [InlineData("Contoso Ltd", "Contoso Ltd")]
    [InlineData("", "Unknown publisher")]
    [InlineData(null, "Unknown publisher")]
    public void PublisherText_FallsBack(string? publisher, string expected)
    {
        Assert.Equal(expected, R2Formatter.PublisherText(publisher));
    }

    [Theory]
    [InlineData("Contoso.App_1.0.0.0_x64__abc", "Contoso.App_1.0.0.0_x64__abc")]
    [InlineData("", "Desktop app")]
    [InlineData(null, "Desktop app")]
    public void PackageText_EmptyIsDesktop(string? identity, string expected)
    {
        Assert.Equal(expected, R2Formatter.PackageText(identity));
    }

    [Fact]
    public void UserText_PrefersNameThenSidThenUnknown()
    {
        Assert.Equal("CONTOSO\\alice", R2Formatter.UserText("CONTOSO\\alice", "S-1-5-21-1"));
        Assert.Equal("S-1-5-21-1", R2Formatter.UserText("", "S-1-5-21-1"));
        Assert.Equal("Unknown", R2Formatter.UserText(null, null));
    }

    [Theory]
    [InlineData("value", "value")]
    [InlineData("", "—")]
    [InlineData(null, "—")]
    public void OrDash_BlankBecomesDash(string? value, string expected)
    {
        Assert.Equal(expected, R2Formatter.OrDash(value));
    }

    // ---- Addresses / handles -----------------------------------------------

    [Fact]
    public void AddressText_ZeroIsDash_NonZeroIsPadded()
    {
        Assert.Equal("—", R2Formatter.AddressText(0));
        Assert.Equal("0x00007FFAB1230000", R2Formatter.AddressText(0x00007FFAB1230000UL));
        Assert.Equal("0x0000000000000001", R2Formatter.AddressText(1));
    }

    [Fact]
    public void HandleText_IsCompactHex()
    {
        Assert.Equal("0x1A4", R2Formatter.HandleText(0x1A4));
        Assert.Equal("0x0", R2Formatter.HandleText(0));
    }

    [Fact]
    public void GrantedAccessText_IsCompactHex()
    {
        Assert.Equal("0x1F0FFF", R2Formatter.GrantedAccessText(0x1F0FFF));
        Assert.Equal("0x0", R2Formatter.GrantedAccessText(0));
    }

    [Fact]
    public void AccessRightsSummary_DecodesStandardRights()
    {
        // SYNCHRONIZE | READ_CONTROL
        Assert.Equal("Read control, Synchronize", R2Formatter.AccessRightsSummary(0x00120000));
        // All standard rights set → "Full control"
        Assert.Equal("Full control", R2Formatter.AccessRightsSummary(0x001F0000));
        // No standard rights → empty (hex stands alone)
        Assert.Equal(string.Empty, R2Formatter.AccessRightsSummary(0x00000001));
    }

    [Fact]
    public void AccessRightsSummary_DecodesGenericRights()
    {
        Assert.Contains("Generic all", R2Formatter.AccessRightsSummary(0x10000000));
        Assert.Contains("Generic read", R2Formatter.AccessRightsSummary(0x80000000));
    }

    [Theory]
    [InlineData("File", "File")]
    [InlineData("", "—")]
    public void HandleTypeText_BlankIsDash(string? type, string expected)
    {
        Assert.Equal(expected, R2Formatter.HandleTypeText(type));
    }

    [Theory]
    [InlineData("\\Device\\Foo", "\\Device\\Foo")]
    [InlineData("", "(unnamed)")]
    [InlineData(null, "(unnamed)")]
    public void HandleNameText_BlankIsUnnamed(string? name, string expected)
    {
        Assert.Equal(expected, R2Formatter.HandleNameText(name));
    }

    // ---- Sizes -------------------------------------------------------------

    [Theory]
    [InlineData(0UL, "—")]
    [InlineData(512UL, "512 B")]
    [InlineData(4096UL, "4 KB")]
    [InlineData(1536UL, "1.5 KB")]
    [InlineData(1048576UL, "1 MB")]
    [InlineData(1610612736UL, "1.5 GB")]
    public void ByteSizeText_AutoScales(ulong bytes, string expected)
    {
        Assert.Equal(expected, R2Formatter.ByteSizeText(bytes));
    }

    // ---- Threads -----------------------------------------------------------

    [Theory]
    [InlineData("Waiting", "Waiting")]
    [InlineData("", "Unknown")]
    [InlineData(null, "Unknown")]
    public void ThreadStateLabel_BlankIsUnknown(string? state, string expected)
    {
        Assert.Equal(expected, R2Formatter.ThreadStateLabel(state));
    }

    [Theory]
    [InlineData("UserRequest", "UserRequest")]
    [InlineData("", "—")]
    public void WaitReasonText_BlankIsDash(string? reason, string expected)
    {
        Assert.Equal(expected, R2Formatter.WaitReasonText(reason));
    }

    [Theory]
    [InlineData(0U, "0%")]
    [InlineData(123U, "12.3%")]
    [InlineData(1000U, "100%")]
    public void CpuPermilleText_Percent(uint permille, string expected)
    {
        Assert.Equal(expected, R2Formatter.CpuPermilleText(permille));
    }

    [Fact]
    public void CpuTimeText_ScalesFromFiletimeTicks()
    {
        Assert.Equal("0 ms", R2Formatter.CpuTimeText(0, 0));
        // 5,000,000 100ns ticks = 500 ms total
        Assert.Equal("500 ms", R2Formatter.CpuTimeText(3_000_000, 2_000_000));
        // 25,000,000 ticks = 2.5 s
        Assert.Equal("2.5 s", R2Formatter.CpuTimeText(25_000_000, 0));
        // 900,000,000 ticks = 90 s = 1m 30s
        Assert.Equal("1m 30s", R2Formatter.CpuTimeText(900_000_000, 0));
    }

    // ---- Coverage notes (the honesty surface) ------------------------------

    [Fact]
    public void LimitedCoverageNote_OnlyWhenLimited()
    {
        Assert.Equal(string.Empty, R2Formatter.LimitedCoverageNote(false));
        var note = R2Formatter.LimitedCoverageNote(true);
        Assert.Contains("run as administrator", note);
        Assert.Equal(R2Formatter.LimitedCoverageMessage, note);
    }

    [Fact]
    public void NamesLimitedNote_ReassuresHandlesStillListed()
    {
        Assert.Equal(string.Empty, R2Formatter.NamesLimitedNote(false));
        var note = R2Formatter.NamesLimitedNote(true);
        Assert.Contains("still listed", note);
    }

    [Fact]
    public void UnavailableReason_PrefersServiceReason()
    {
        Assert.Equal("process exited",
            R2Formatter.UnavailableReason("process exited", "fallback"));
        Assert.Equal("fallback",
            R2Formatter.UnavailableReason("", "fallback"));
        Assert.Equal("fallback",
            R2Formatter.UnavailableReason(null, "fallback"));
    }
}
