using System.Text.Json;
using Atlas.IpcClient;
using Atlas.V0;
using Xunit;

namespace Atlas.IpcClient.Tests;

public sealed class GamingRecordingExporterTests
{
    [Fact]
    public void JsonContainsSessionSummaryLimitationsAndRetainedSamples()
    {
        var session = Session();
        var samples = new[] { Sample("round, one") };

        var text = GamingRecordingExporter.Build(
            session,
            samples,
            GamingRecordingFormat.Json,
            DateTimeOffset.Parse("2026-07-21T01:10:00Z"));
        using var document = JsonDocument.Parse(text);
        var root = document.RootElement;

        Assert.Equal("system-atlas-gaming-recording", root.GetProperty("format").GetString());
        Assert.Equal(1, root.GetProperty("schemaVersion").GetInt32());
        Assert.Equal("Valorant, Competitive", root.GetProperty("session").GetProperty("gameName").GetString());
        Assert.Equal(143.9, root.GetProperty("session").GetProperty("summary").GetProperty("averageFps").GetDouble());
        Assert.Equal("diagnostic evidence", root.GetProperty("session").GetProperty("limitations")[0].GetString());
        Assert.Equal(1, root.GetProperty("samples").GetArrayLength());
        Assert.Equal(7.9, root.GetProperty("samples")[0].GetProperty("frameTimeP95Ms").GetDouble());
    }

    [Fact]
    public void CsvIsInvariantSpreadsheetFriendlyAndEscapesText()
    {
        var text = GamingRecordingExporter.Build(
            Session(),
            new[] { Sample("round, \"one\"") },
            GamingRecordingFormat.Csv,
            DateTimeOffset.Parse("2026-07-21T01:10:00Z"));
        var lines = text.Split(Environment.NewLine, StringSplitOptions.RemoveEmptyEntries);

        Assert.Equal(2, lines.Length);
        Assert.Contains("sample_frame_time_p95_ms", lines[0], StringComparison.Ordinal);
        Assert.Contains("143.9", lines[1], StringComparison.Ordinal);
        Assert.Contains("7.9", lines[1], StringComparison.Ordinal);
        Assert.Contains("\"round, \"\"one\"\"\"", lines[1], StringComparison.Ordinal);
    }

    [Fact]
    public void CsvRetainsSessionMetadataWhenThereAreNoSamples()
    {
        var text = GamingRecordingExporter.Build(
            Session(),
            Array.Empty<GamingTraceBucket>(),
            GamingRecordingFormat.Csv,
            DateTimeOffset.Parse("2026-07-21T01:10:00Z"));

        Assert.Equal(2, text.Split(Environment.NewLine, StringSplitOptions.RemoveEmptyEntries).Length);
        Assert.Contains("Valorant, Competitive", text, StringComparison.Ordinal);
    }

    [Fact]
    public void SuggestedNameIsSafeAndIdentifiable()
    {
        var localStart = DateTimeOffset.Parse("2026-07-21T01:06:21Z")
            .ToLocalTime()
            .ToString("yyyyMMdd-HHmmss");
        Assert.Equal(
            $"system-atlas-valorant-competitive-{localStart}",
            GamingRecordingExporter.SuggestedFileName(Session()));
    }

    private static GameSession Session()
    {
        var session = new GameSession
        {
            Id = 42,
            GameId = "valorant",
            GameName = "Valorant, Competitive",
            Objective = GamingObjective.CompetitiveLatency,
            ProcessId = 9001,
            ProcessCreateTime100Ns = 12345,
            StartMs = DateTimeOffset.Parse("2026-07-21T01:06:21Z").ToUnixTimeMilliseconds(),
            EndMs = DateTimeOffset.Parse("2026-07-21T01:07:37Z").ToUnixTimeMilliseconds(),
            CaptureQuality = GamingCaptureQuality.Partial,
            ConfigurationSnapshotHash = "abc123",
            Comparable = false,
            Summary = new GameSessionSummary
            {
                AverageFps = 143.9,
                OnePercentLowFps = 106.8,
                FrameTimeP95Ms = 7.9,
                LongFrameCount = 0,
                CpuAveragePercent = 33.2,
                GpuAveragePercent = 30.7,
                RamPeakBytes = 12_100_000_000,
                TemperaturePeakC = 68,
            },
        };
        session.Limitations.Add("diagnostic evidence");
        session.Summary.Limitations.Add("anti-cheat matrix pending");
        return session;
    }

    private static GamingTraceBucket Sample(string label) => new()
    {
        TsMs = DateTimeOffset.Parse("2026-07-21T01:06:21Z").ToUnixTimeMilliseconds(),
        FrameTimeMs = 7.9,
        CpuPercent = 36.4,
        GpuPercent = 28.1,
        RamUsedBytes = 12_100_000_000,
        TemperatureC = 65,
        BackgroundProcesses = 118,
        EventLabel = label,
    };
}
