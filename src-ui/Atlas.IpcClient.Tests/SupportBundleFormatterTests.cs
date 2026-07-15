using System;
using System.Linq;
using Atlas.IpcClient;
using Atlas.V0;
using Xunit;

namespace Atlas.IpcClient.Tests;

public class SupportBundleFormatterTests
{
    // ---- Section catalog ---------------------------------------------------

    [Fact]
    public void AllSections_CoversTheEightRealSections_WithoutUnspecified()
    {
        Assert.Equal(8, SupportBundleFormatter.AllSections.Count);
        Assert.DoesNotContain(
            SupportBundleSection.Unspecified,
            SupportBundleFormatter.AllSections);
        // Distinct, and matches every non-zero proto enum value.
        var fromProto = Enum.GetValues<SupportBundleSection>()
            .Where(s => s != SupportBundleSection.Unspecified)
            .OrderBy(s => (int)s);
        Assert.Equal(fromProto, SupportBundleFormatter.AllSections.OrderBy(s => (int)s));
    }

    [Theory]
    [InlineData(SupportBundleSection.BundleDeviceInfo, "Device info")]
    [InlineData(SupportBundleSection.BundleHealth, "Health")]
    [InlineData(SupportBundleSection.BundleIncidents, "Incidents & diagnoses")]
    [InlineData(SupportBundleSection.BundleSystemChanges, "System changes")]
    [InlineData(SupportBundleSection.BundleCrashes, "Crashes")]
    [InlineData(SupportBundleSection.BundleServices, "Services")]
    [InlineData(SupportBundleSection.BundleStartup, "Startup")]
    [InlineData(SupportBundleSection.BundleSelfMetrics, "Atlas overhead")]
    public void SectionLabel_MapsSections(SupportBundleSection section, string expected)
    {
        Assert.Equal(expected, SupportBundleFormatter.SectionLabel(section));
    }

    [Fact]
    public void SectionDescription_IsNonEmptyForEverySection()
    {
        foreach (var section in SupportBundleFormatter.AllSections)
        {
            Assert.False(
                string.IsNullOrWhiteSpace(SupportBundleFormatter.SectionDescription(section)),
                $"description missing for {section}");
        }
    }

    [Fact]
    public void SectionGlyph_IsNonEmptyForEverySection()
    {
        foreach (var section in SupportBundleFormatter.AllSections)
        {
            Assert.False(string.IsNullOrEmpty(SupportBundleFormatter.SectionGlyph(section)));
        }
    }

    [Theory]
    [InlineData(SupportBundleSection.BundleIncidents, true)]
    [InlineData(SupportBundleSection.BundleSystemChanges, true)]
    [InlineData(SupportBundleSection.BundleCrashes, true)]
    [InlineData(SupportBundleSection.BundleDeviceInfo, false)]
    [InlineData(SupportBundleSection.BundleHealth, false)]
    [InlineData(SupportBundleSection.BundleServices, false)]
    [InlineData(SupportBundleSection.BundleStartup, false)]
    [InlineData(SupportBundleSection.BundleSelfMetrics, false)]
    public void IsWindowed_MarksOnlyTimeBoundedSections(SupportBundleSection section, bool expected)
    {
        Assert.Equal(expected, SupportBundleFormatter.IsWindowed(section));
    }

    [Fact]
    public void WindowedSections_MatchIsWindowed()
    {
        foreach (var section in SupportBundleFormatter.AllSections)
        {
            var inList = SupportBundleFormatter.WindowedSections.Contains(section);
            Assert.Equal(SupportBundleFormatter.IsWindowed(section), inList);
        }
    }

    // ---- Time window -------------------------------------------------------

    [Theory]
    [InlineData(24, "Last 24 hours")]
    [InlineData(72, "Last 72 hours")]
    [InlineData(168, "Last 7 days")]
    [InlineData(48, "Last 2 days")]
    [InlineData(5, "Last 5 hours")]
    public void WindowLabel_MapsCommonSpans(int hours, string expected)
    {
        Assert.Equal(expected, SupportBundleFormatter.WindowLabel(hours));
    }

    [Fact]
    public void WindowFromMs_SubtractsTheSpan()
    {
        long now = 1_000_000_000L;
        Assert.Equal(now - 24L * 3_600_000L, SupportBundleFormatter.WindowFromMs(now, 24));
        Assert.Equal(now - 72L * 3_600_000L, SupportBundleFormatter.WindowFromMs(now, 72));
        Assert.Equal(now - 168L * 3_600_000L, SupportBundleFormatter.WindowFromMs(now, 168));
    }

    [Fact]
    public void WindowFromMs_ClampsAtZero()
    {
        Assert.Equal(0, SupportBundleFormatter.WindowFromMs(1000, 168));
    }

    // ---- Redaction summary (pre-generate) ----------------------------------

    [Fact]
    public void RedactionSummary_AllOn_ListsEveryCategory()
    {
        var options = new RedactionOptions
        {
            RedactUserNames = true,
            RedactComputerName = true,
            RedactPaths = true,
            RedactCommandLines = true,
        };
        var text = SupportBundleFormatter.RedactionSummary(options);
        Assert.Contains("user names", text);
        Assert.Contains("the computer name", text);
        Assert.Contains("file paths", text);
        Assert.Contains("command lines", text);
        Assert.Contains("before the file is created", text.ToLowerInvariant());
    }

    [Fact]
    public void RedactionSummary_NoneOn_WarnsThatNothingIsRemoved()
    {
        var text = SupportBundleFormatter.RedactionSummary(new RedactionOptions());
        Assert.Contains("Nothing will be removed", text);
        Assert.Contains("in full", text);
    }

    [Fact]
    public void RedactionSummary_SubsetUsesOxfordJoin()
    {
        var options = new RedactionOptions
        {
            RedactUserNames = true,
            RedactPaths = true,
        };
        var text = SupportBundleFormatter.RedactionSummary(options);
        Assert.Equal(
            "Before the file is created, Atlas will remove: user names and file paths.",
            text);
    }

    // ---- Redaction applied (post-generate echo) ----------------------------

    [Fact]
    public void RedactionAppliedSummary_ListsWhatWasStripped()
    {
        var text = SupportBundleFormatter.RedactionAppliedSummary(
            new[] { "user names", "file paths", "command lines" });
        Assert.Equal(
            "Removed before this file was created: user names, file paths, and command lines.",
            text);
    }

    [Fact]
    public void RedactionAppliedSummary_EmptyList_SaysNoneApplied()
    {
        Assert.Equal(
            "No redaction was applied — this bundle includes all details.",
            SupportBundleFormatter.RedactionAppliedSummary(Array.Empty<string>()));
        Assert.Equal(
            "No redaction was applied — this bundle includes all details.",
            SupportBundleFormatter.RedactionAppliedSummary(null));
    }

    [Fact]
    public void RedactionAppliedSummary_TrimsAndSkipsBlankEntries()
    {
        var text = SupportBundleFormatter.RedactionAppliedSummary(
            new[] { "  user names ", "", "   ", "file paths" });
        Assert.Equal(
            "Removed before this file was created: user names and file paths.",
            text);
    }

    [Fact]
    public void RedactionAppliedSummary_SingleCategory_NoJoinWord()
    {
        Assert.Equal(
            "Removed before this file was created: the computer name.",
            SupportBundleFormatter.RedactionAppliedSummary(new[] { "the computer name" }));
    }
}
