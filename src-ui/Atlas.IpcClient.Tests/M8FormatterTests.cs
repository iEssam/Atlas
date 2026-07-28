using Atlas.IpcClient;
using Atlas.V0;
using Xunit;

namespace Atlas.IpcClient.Tests;

public class M8FormatterTests
{
    // ---- Incident kind -----------------------------------------------------

    [Theory]
    [InlineData(IncidentKind.CpuSaturation, "CPU saturation")]
    [InlineData(IncidentKind.MemoryPressure, "Memory pressure")]
    [InlineData(IncidentKind.DiskLatency, "Disk latency")]
    [InlineData(IncidentKind.GpuSaturation, "GPU saturation")]
    [InlineData(IncidentKind.GpuMemoryExhaustion, "GPU memory pressure")]
    [InlineData(IncidentKind.GpuThermalThrottling, "GPU thermal throttling")]
    [InlineData(IncidentKind.SystemThermalLimit, "System thermal limit")]
    [InlineData(IncidentKind.Unspecified, "Incident")]
    public void IncidentKindLabel_MapsKinds(IncidentKind kind, string expected)
    {
        Assert.Equal(expected, M8Formatter.IncidentKindLabel(kind));
    }

    [Fact]
    public void IncidentKindGlyph_IsNonEmptyPerKind()
    {
        Assert.False(string.IsNullOrEmpty(M8Formatter.IncidentKindGlyph(IncidentKind.CpuSaturation)));
        Assert.False(string.IsNullOrEmpty(M8Formatter.IncidentKindGlyph(IncidentKind.MemoryPressure)));
        Assert.False(string.IsNullOrEmpty(M8Formatter.IncidentKindGlyph(IncidentKind.DiskLatency)));
        Assert.False(string.IsNullOrEmpty(M8Formatter.IncidentKindGlyph(IncidentKind.SystemThermalLimit)));
        Assert.False(string.IsNullOrEmpty(M8Formatter.IncidentKindGlyph(IncidentKind.Unspecified)));
    }

    [Theory]
    [InlineData(IncidentKind.CpuSaturation, "CPU")]
    [InlineData(IncidentKind.MemoryPressure, "memory")]
    [InlineData(IncidentKind.DiskLatency, "disk")]
    [InlineData(IncidentKind.SystemThermalLimit, "system thermal state")]
    public void ResourceNoun_MapsKinds(IncidentKind kind, string expected)
    {
        Assert.Equal(expected, M8Formatter.ResourceNoun(kind));
    }

    [Fact]
    public void PeakValueText_CpuAndMemoryArePercent_DiskIsMs()
    {
        Assert.Equal("Peaked at 97.3% CPU", M8Formatter.PeakValueText(IncidentKind.CpuSaturation, 973));
        Assert.Equal("Peaked at 88% memory", M8Formatter.PeakValueText(IncidentKind.MemoryPressure, 880));
        Assert.Equal("Peaked at 42.5 ms latency", M8Formatter.PeakValueText(IncidentKind.DiskLatency, 42.5));
        Assert.Equal("Peaked at 91.5 °C", M8Formatter.PeakValueText(IncidentKind.SystemThermalLimit, 91.5));
        Assert.Equal(string.Empty, M8Formatter.PeakValueText(IncidentKind.Unspecified, 5));
    }

    // ---- Severity (the danger scale) --------------------------------------

    [Theory]
    [InlineData(Severity.Info, "Info")]
    [InlineData(Severity.Warning, "Warning")]
    [InlineData(Severity.Critical, "Critical")]
    [InlineData(Severity.Unspecified, "Unknown")]
    public void SeverityLabel_MapsSeverities(Severity severity, string expected)
    {
        Assert.Equal(expected, M8Formatter.SeverityLabel(severity));
    }

    [Theory]
    [InlineData(Severity.Info, "info")]
    [InlineData(Severity.Warning, "warning")]
    [InlineData(Severity.Critical, "critical")]
    [InlineData(Severity.Unspecified, "unknown")]
    public void SeverityColorToken_MapsToTokens(Severity severity, string expected)
    {
        Assert.Equal(expected, M8Formatter.SeverityColorToken(severity));
    }

    // ---- Confidence (the calm, epistemic scale) ---------------------------

    [Theory]
    [InlineData(Confidence.Confirmed, "Confirmed")]
    [InlineData(Confidence.High, "High confidence")]
    [InlineData(Confidence.Medium, "Medium confidence")]
    [InlineData(Confidence.Low, "Low confidence")]
    [InlineData(Confidence.Insufficient, "Insufficient evidence")]
    [InlineData(Confidence.Unspecified, "Unknown")]
    public void ConfidenceLabel_MapsRungs(Confidence confidence, string expected)
    {
        Assert.Equal(expected, M8Formatter.ConfidenceLabel(confidence));
    }

    [Theory]
    [InlineData(Confidence.Confirmed, "confirmed")]
    [InlineData(Confidence.High, "high")]
    [InlineData(Confidence.Medium, "medium")]
    [InlineData(Confidence.Low, "low")]
    [InlineData(Confidence.Insufficient, "insufficient")]
    [InlineData(Confidence.Unspecified, "unknown")]
    public void ConfidenceColorToken_MapsToCalmTokens(Confidence confidence, string expected)
    {
        Assert.Equal(expected, M8Formatter.ConfidenceColorToken(confidence));
    }

    [Fact]
    public void ConfidenceColorToken_LowIsNotADangerToken()
    {
        // Epistemic honesty, not alarm: low/insufficient must never share a token
        // with the incident danger scale ("critical"/"warning").
        var low = M8Formatter.ConfidenceColorToken(Confidence.Low);
        var insufficient = M8Formatter.ConfidenceColorToken(Confidence.Insufficient);
        Assert.NotEqual("critical", low);
        Assert.NotEqual("warning", low);
        Assert.NotEqual("critical", insufficient);
        Assert.NotEqual("warning", insufficient);
    }

    // ---- Attribution -------------------------------------------------------

    [Theory]
    [InlineData(0.723, "~72%")]
    [InlineData(0.725, "~73%")]  // rounds away from zero
    [InlineData(1.0, "~100%")]
    [InlineData(1.5, "~100%")]   // clamped
    public void AttributionShare_RoundsAndClamps(double attribution, string expected)
    {
        Assert.Equal(expected, M8Formatter.AttributionShare(attribution));
    }

    [Theory]
    [InlineData(0.0)]
    [InlineData(-0.2)]
    public void AttributionShare_NonPositiveIsEmpty(double attribution)
    {
        Assert.Equal(string.Empty, M8Formatter.AttributionShare(attribution));
    }

    [Fact]
    public void AttributionText_TiesShareToResource()
    {
        Assert.Equal("~72% of CPU", M8Formatter.AttributionText(0.72, IncidentKind.CpuSaturation));
        Assert.Equal("~30% of memory", M8Formatter.AttributionText(0.30, IncidentKind.MemoryPressure));
        Assert.Equal(string.Empty, M8Formatter.AttributionText(0, IncidentKind.CpuSaturation));
    }

    [Fact]
    public void ProcessText_CombinesNameAndPid()
    {
        Assert.Equal("chrome.exe (pid 4242)", M8Formatter.ProcessText("chrome.exe", 4242));
        Assert.Equal("chrome.exe", M8Formatter.ProcessText("chrome.exe", 0));
        Assert.Equal("pid 900", M8Formatter.ProcessText("", 900));
        Assert.Equal(string.Empty, M8Formatter.ProcessText(null, 0));
    }

    // ---- Durations / windows ----------------------------------------------

    [Theory]
    [InlineData(0, "0s")]
    [InlineData(-5, "0s")]
    [InlineData(45_000, "45s")]
    [InlineData(5 * 60_000, "5m")]
    [InlineData(5 * 60_000 + 20_000, "5m 20s")]
    [InlineData(60 * 60_000, "1h")]
    [InlineData(63 * 60_000, "1h 3m")]
    [InlineData(26 * 60 * 60_000L, "1d 2h")]
    [InlineData(48 * 60 * 60_000L, "2d")]
    public void DurationText_Buckets(long ms, string expected)
    {
        Assert.Equal(expected, M8Formatter.DurationText(ms));
    }

    [Fact]
    public void IncidentWindowText_OngoingVsClosed()
    {
        long now = 100_000_000_000;
        long start = now - 2 * 60 * 60 * 1000; // 2h ago
        Assert.Equal("Started 2h ago • ongoing", M8Formatter.IncidentWindowText(start, 0, now));

        long end = start + 5 * 60 * 1000; // lasted 5m
        Assert.Equal("Started 2h ago • lasted 5m", M8Formatter.IncidentWindowText(start, end, now));
    }

    [Fact]
    public void WindowRangeText_ClosedUsesDuration()
    {
        // A pure localizer so the assertion is deterministic.
        string Loc(long ms) => ms.ToString();
        long start = 1000;
        long end = start + 5 * 60_000; // 5m
        Assert.Equal("1000 – 301000 (5m)", M8Formatter.WindowRangeText(start, end, 999_999, Loc));
    }

    [Fact]
    public void WindowRangeText_OpenEndsAtNow()
    {
        string Loc(long ms) => ms.ToString();
        long start = 1000;
        long now = start + 60_000; // 1m later
        Assert.Equal("1000 – 61000 (ongoing)", M8Formatter.WindowRangeText(start, 0, now, Loc));
    }

    // ---- Evidence ----------------------------------------------------------

    [Fact]
    public void EvidenceMetricText_TagsMetricAndValue()
    {
        Assert.Equal("sys.cpu = 97.3", M8Formatter.EvidenceMetricText("sys.cpu", 97.3));
        Assert.Equal(string.Empty, M8Formatter.EvidenceMetricText("", 5));
        Assert.Equal(string.Empty, M8Formatter.EvidenceMetricText(null, 5));
    }

    // ---- Reports -----------------------------------------------------------

    [Theory]
    [InlineData(ReportFormat.ReportHtml, "HTML")]
    [InlineData(ReportFormat.ReportCsv, "CSV")]
    [InlineData(ReportFormat.ReportJson, "JSON")]
    [InlineData(ReportFormat.ReportText, "Plain text")]
    public void ReportFormatLabel_MapsFormats(ReportFormat format, string expected)
    {
        Assert.Equal(expected, M8Formatter.ReportFormatLabel(format));
    }

    [Theory]
    [InlineData(ReportFormat.ReportHtml, "html")]
    [InlineData(ReportFormat.ReportCsv, "csv")]
    [InlineData(ReportFormat.ReportJson, "json")]
    [InlineData(ReportFormat.ReportText, "txt")]
    public void ReportFormatExtension_MapsFormats(ReportFormat format, string expected)
    {
        Assert.Equal(expected, M8Formatter.ReportFormatExtension(format));
    }

    [Fact]
    public void RedactionSummary_NoneSelected_SaysNothingRemoved()
    {
        var summary = M8Formatter.RedactionSummary(new RedactionOptions());
        Assert.Contains("Nothing will be removed", summary);
    }

    [Fact]
    public void RedactionSummary_ListsSelectedItems()
    {
        var options = new RedactionOptions
        {
            RedactUserNames = true,
            RedactPaths = true,
        };
        var summary = M8Formatter.RedactionSummary(options);
        Assert.Contains("user names", summary);
        Assert.Contains("file paths", summary);
        Assert.DoesNotContain("computer name", summary);
        Assert.DoesNotContain("command lines", summary);
    }

    [Fact]
    public void RedactionSummary_AllSelected()
    {
        var options = new RedactionOptions
        {
            RedactUserNames = true,
            RedactComputerName = true,
            RedactPaths = true,
            RedactCommandLines = true,
        };
        var summary = M8Formatter.RedactionSummary(options);
        Assert.Contains("user names", summary);
        Assert.Contains("computer name", summary);
        Assert.Contains("file paths", summary);
        Assert.Contains("command lines", summary);
    }
}
