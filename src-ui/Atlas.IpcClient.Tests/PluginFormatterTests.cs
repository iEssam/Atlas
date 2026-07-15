using System.Linq;
using Atlas.IpcClient;
using Atlas.V0;
using Xunit;

namespace Atlas.IpcClient.Tests;

public class PluginFormatterTests
{
    // ---- Signature ---------------------------------------------------------

    [Theory]
    [InlineData(PluginSignature.PluginSigned, "Signed")]
    [InlineData(PluginSignature.PluginUnsigned, "Unsigned")]
    [InlineData(PluginSignature.PluginSigUnknown, "Unknown")]
    [InlineData(PluginSignature.Unspecified, "Unknown")]
    public void SignatureLabel_MapsSignatures(PluginSignature sig, string expected)
    {
        Assert.Equal(expected, PluginFormatter.SignatureLabel(sig));
    }

    [Theory]
    [InlineData(PluginSignature.PluginSigned, "signed")]
    [InlineData(PluginSignature.PluginUnsigned, "unsigned")]
    [InlineData(PluginSignature.PluginSigUnknown, "unknown")]
    [InlineData(PluginSignature.Unspecified, "unknown")]
    public void SignatureColorToken_MapsToCalmTokens(PluginSignature sig, string expected)
    {
        Assert.Equal(expected, PluginFormatter.SignatureColorToken(sig));
    }

    [Fact]
    public void SignatureColorToken_UnsignedIsNotACriticalDangerToken()
    {
        // Unsigned must read as a caution, never as the red danger palette used by
        // genuinely critical incidents. Guard against a regression that maps it to
        // "critical"/"error"/"danger".
        var token = PluginFormatter.SignatureColorToken(PluginSignature.PluginUnsigned);
        Assert.Equal("unsigned", token);
        Assert.DoesNotContain(token, new[] { "critical", "error", "danger" });
    }

    [Fact]
    public void SignatureNote_UnsignedIsACautionNamingTrust()
    {
        var note = PluginFormatter.SignatureNote(PluginSignature.PluginUnsigned);
        Assert.Contains("Unsigned", note);
        Assert.Contains("trust", note);
    }

    [Fact]
    public void SignatureNote_SignedIsPositive()
    {
        Assert.Contains("verified", PluginFormatter.SignatureNote(PluginSignature.PluginSigned));
    }

    [Fact]
    public void SignatureGlyph_IsNonEmptyPerSignature()
    {
        Assert.False(string.IsNullOrEmpty(PluginFormatter.SignatureGlyph(PluginSignature.PluginSigned)));
        Assert.False(string.IsNullOrEmpty(PluginFormatter.SignatureGlyph(PluginSignature.PluginUnsigned)));
        Assert.False(string.IsNullOrEmpty(PluginFormatter.SignatureGlyph(PluginSignature.PluginSigUnknown)));
    }

    // ---- Capabilities ------------------------------------------------------

    [Fact]
    public void AllCapabilities_AreTheSevenReadOnlyGroups_InOrder()
    {
        Assert.Equal(
            new[]
            {
                PluginCapability.PluginCapSnapshot,
                PluginCapability.PluginCapHistory,
                PluginCapability.PluginCapSearch,
                PluginCapability.PluginCapIncidents,
                PluginCapability.PluginCapInventory,
                PluginCapability.PluginCapNetwork,
                PluginCapability.PluginCapForensics,
            },
            PluginFormatter.AllCapabilities.ToArray());
    }

    [Theory]
    [InlineData(PluginCapability.PluginCapSnapshot, "Live snapshot")]
    [InlineData(PluginCapability.PluginCapHistory, "History")]
    [InlineData(PluginCapability.PluginCapSearch, "Search")]
    [InlineData(PluginCapability.PluginCapIncidents, "Incidents")]
    [InlineData(PluginCapability.PluginCapInventory, "Inventory")]
    [InlineData(PluginCapability.PluginCapNetwork, "Network")]
    [InlineData(PluginCapability.PluginCapForensics, "Forensics")]
    public void CapabilityLabel_MapsCapabilities(PluginCapability cap, string expected)
    {
        Assert.Equal(expected, PluginFormatter.CapabilityLabel(cap));
    }

    [Fact]
    public void CapabilityDescription_IsNonEmptyAndReadOnlyFramedForEveryGroup()
    {
        foreach (var cap in PluginFormatter.AllCapabilities)
        {
            var desc = PluginFormatter.CapabilityDescription(cap);
            Assert.False(string.IsNullOrEmpty(desc));
            // Every capability is a READ-only slice; the copy must say so.
            Assert.Contains("Read", desc, System.StringComparison.OrdinalIgnoreCase);
        }
    }

    [Fact]
    public void NormalizeCapabilities_DeDupesAndSortsIntoCanonicalOrder()
    {
        var input = new[]
        {
            PluginCapability.PluginCapNetwork,
            PluginCapability.PluginCapSnapshot,
            PluginCapability.PluginCapNetwork, // dup
            PluginCapability.Unspecified,       // dropped
        };
        var result = PluginFormatter.NormalizeCapabilities(input);
        Assert.Equal(
            new[] { PluginCapability.PluginCapSnapshot, PluginCapability.PluginCapNetwork },
            result.ToArray());
    }

    [Fact]
    public void NormalizeCapabilities_NullIsEmpty()
    {
        Assert.Empty(PluginFormatter.NormalizeCapabilities(null));
    }

    [Fact]
    public void GrantedSummary_EmptyIsTheSafeDefault()
    {
        Assert.Equal(
            "No access granted — this plugin can't read anything.",
            PluginFormatter.GrantedSummary(System.Array.Empty<PluginCapability>()));
        Assert.Equal(
            "No access granted — this plugin can't read anything.",
            PluginFormatter.GrantedSummary(null));
    }

    [Fact]
    public void GrantedSummary_ListsLabelsInCanonicalOrder()
    {
        var summary = PluginFormatter.GrantedSummary(new[]
        {
            PluginCapability.PluginCapNetwork,
            PluginCapability.PluginCapSnapshot,
        });
        Assert.Equal("Can read: Live snapshot, Network.", summary);
    }

    [Theory]
    [InlineData(0, "No capabilities")]
    [InlineData(1, "1 of 7 read-only capability")]
    [InlineData(3, "3 of 7 read-only capabilities")]
    public void GrantedCountText_Counts(int count, string expected)
    {
        var caps = PluginFormatter.AllCapabilities.Take(count);
        Assert.Equal(expected, PluginFormatter.GrantedCountText(caps));
    }

    // ---- Plugin one-liners -------------------------------------------------

    [Fact]
    public void PluginName_FallsBackWhenBlank()
    {
        Assert.Equal("(unnamed plugin)", PluginFormatter.PluginName(new Plugin()));
        Assert.Equal("Timeline Insights", PluginFormatter.PluginName(new Plugin { Name = "Timeline Insights" }));
    }

    [Theory]
    [InlineData("1.2.0", "v1.2.0")]
    [InlineData("", "")]
    [InlineData("   ", "")]
    public void VersionText_Formats(string version, string expected)
    {
        Assert.Equal(expected, PluginFormatter.VersionText(version));
    }

    [Theory]
    [InlineData("Contoso Ltd.", "Contoso Ltd.")]
    [InlineData("", "Unknown publisher")]
    [InlineData(null, "Unknown publisher")]
    public void PublisherText_FallsBackPlainly(string? publisher, string expected)
    {
        Assert.Equal(expected, PluginFormatter.PublisherText(publisher));
    }

    [Fact]
    public void PluginSummary_TiesTogetherNameVersionSignatureStateAndBreadth()
    {
        var plugin = new Plugin
        {
            Name = "Timeline Insights",
            Version = "1.2.0",
            Signature = PluginSignature.PluginSigned,
            Enabled = true,
            Granted =
            {
                PluginCapability.PluginCapSnapshot,
                PluginCapability.PluginCapHistory,
                PluginCapability.PluginCapNetwork,
            },
        };
        Assert.Equal(
            "Timeline Insights v1.2.0 • Signed • On • 3 of 7 read-only capabilities",
            PluginFormatter.PluginSummary(plugin));
    }

    [Fact]
    public void PluginSummary_DisabledUnsignedReadsCalmly()
    {
        var plugin = new Plugin
        {
            Name = "Scratch tool",
            Signature = PluginSignature.PluginUnsigned,
            Enabled = false,
        };
        Assert.Equal("Scratch tool • Unsigned • Off • No capabilities", PluginFormatter.PluginSummary(plugin));
    }
}
