using System.Globalization;
using System.Text;
using System.Text.Json;
using Atlas.V0;

namespace Atlas.IpcClient;

public enum GamingRecordingFormat
{
    Json,
    Csv,
}

/// <summary>Creates portable files from one retained gaming recording.</summary>
public static class GamingRecordingExporter
{
    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        WriteIndented = true,
    };

    public static string Build(
        GameSession session,
        IEnumerable<GamingTraceBucket> samples,
        GamingRecordingFormat format,
        DateTimeOffset? exportedAt = null)
    {
        ArgumentNullException.ThrowIfNull(session);
        ArgumentNullException.ThrowIfNull(samples);

        var retainedSamples = samples.ToArray();
        return format switch
        {
            GamingRecordingFormat.Json => BuildJson(session, retainedSamples, exportedAt ?? DateTimeOffset.UtcNow),
            GamingRecordingFormat.Csv => BuildCsv(session, retainedSamples, exportedAt ?? DateTimeOffset.UtcNow),
            _ => throw new ArgumentOutOfRangeException(nameof(format), format, "Unsupported gaming recording format."),
        };
    }

    public static string SuggestedFileName(GameSession session)
    {
        ArgumentNullException.ThrowIfNull(session);
        var game = FileNamePart(session.GameName);
        var start = UnixTime(session.StartMs)?.ToLocalTime().ToString("yyyyMMdd-HHmmss", CultureInfo.InvariantCulture)
            ?? $"session-{session.Id}";
        return $"system-atlas-{game}-{start}";
    }

    private static string BuildJson(
        GameSession session,
        IReadOnlyCollection<GamingTraceBucket> samples,
        DateTimeOffset exportedAt)
    {
        var document = new RecordingDocument(
            "system-atlas-gaming-recording",
            1,
            exportedAt.ToUniversalTime(),
            SessionDocument.From(session),
            samples.Select(SampleDocument.From).ToArray());
        return JsonSerializer.Serialize(document, JsonOptions) + Environment.NewLine;
    }

    private static string BuildCsv(
        GameSession session,
        IReadOnlyCollection<GamingTraceBucket> samples,
        DateTimeOffset exportedAt)
    {
        var summary = session.Summary;
        var header = new[]
        {
            "schema_version", "exported_utc", "session_id", "game_id", "game_name", "objective",
            "capture_quality", "session_start_utc", "session_end_utc", "process_id",
            "process_create_time_100ns", "configuration_snapshot_hash", "applied_plan_id", "comparable",
            "average_fps", "one_percent_low_fps", "point_one_percent_low_fps", "frame_time_p50_ms",
            "frame_time_p95_ms", "frame_time_p99_ms", "long_frame_count", "cpu_average_percent",
            "gpu_average_percent", "vram_peak_bytes", "ram_peak_bytes", "temperature_peak_c",
            "contention", "incidents", "session_limitations", "summary_limitations", "sample_time_utc",
            "sample_frame_time_p95_ms", "sample_cpu_percent", "sample_gpu_percent",
            "sample_vram_used_bytes", "sample_ram_used_bytes", "sample_temperature_c",
            "sample_disk_bytes_per_sec", "sample_background_processes", "sample_incident",
            "sample_data_gap", "sample_event"
        };

        var text = new StringBuilder();
        AppendCsvRow(text, header);
        if (samples.Count == 0)
        {
            AppendCsvRow(text, CsvValues(session, summary, null, exportedAt));
        }
        else
        {
            foreach (var sample in samples)
            {
                AppendCsvRow(text, CsvValues(session, summary, sample, exportedAt));
            }
        }
        return text.ToString();
    }

    private static object?[] CsvValues(
        GameSession session,
        GameSessionSummary? summary,
        GamingTraceBucket? sample,
        DateTimeOffset exportedAt) =>
    [
        1,
        exportedAt.ToUniversalTime(),
        session.Id,
        session.GameId,
        session.GameName,
        session.Objective.ToString(),
        session.CaptureQuality.ToString(),
        UnixTime(session.StartMs),
        UnixTime(session.EndMs),
        session.ProcessId,
        session.ProcessCreateTime100Ns,
        session.ConfigurationSnapshotHash,
        session.AppliedPlanId,
        session.Comparable,
        summary?.AverageFps,
        summary?.OnePercentLowFps,
        summary?.PointOnePercentLowFps,
        summary?.FrameTimeP50Ms,
        summary?.FrameTimeP95Ms,
        summary?.FrameTimeP99Ms,
        summary?.LongFrameCount,
        summary?.CpuAveragePercent,
        summary?.GpuAveragePercent,
        summary?.VramPeakBytes,
        summary?.RamPeakBytes,
        summary?.TemperaturePeakC,
        summary is null ? string.Empty : string.Join(" | ", summary.Contention),
        summary is null ? string.Empty : string.Join(" | ", summary.Incidents),
        string.Join(" | ", session.Limitations),
        summary is null ? string.Empty : string.Join(" | ", summary.Limitations),
        sample is null ? null : UnixTime(sample.TsMs),
        sample?.FrameTimeMs,
        sample?.CpuPercent,
        sample?.GpuPercent,
        sample?.VramUsedBytes,
        sample?.RamUsedBytes,
        sample?.TemperatureC,
        sample?.DiskBytesPerSec,
        sample?.BackgroundProcesses,
        sample?.Incident,
        sample?.DataGap,
        sample?.EventLabel,
    ];

    private static void AppendCsvRow(StringBuilder text, IEnumerable<object?> values)
    {
        var first = true;
        foreach (var value in values)
        {
            if (!first) text.Append(',');
            text.Append(CsvCell(value));
            first = false;
        }
        text.AppendLine();
    }

    private static string CsvCell(object? value)
    {
        var formatted = value switch
        {
            null => string.Empty,
            DateTimeOffset timestamp => timestamp.ToUniversalTime().ToString("O", CultureInfo.InvariantCulture),
            double number => number.ToString("R", CultureInfo.InvariantCulture),
            float number => number.ToString("R", CultureInfo.InvariantCulture),
            bool flag => flag ? "true" : "false",
            IFormattable formattable => formattable.ToString(null, CultureInfo.InvariantCulture),
            _ => value.ToString() ?? string.Empty,
        };
        return formatted.IndexOfAny([',', '"', '\r', '\n']) < 0
            ? formatted
            : $"\"{formatted.Replace("\"", "\"\"")}\"";
    }

    private static DateTimeOffset? UnixTime(long milliseconds) => milliseconds > 0
        ? DateTimeOffset.FromUnixTimeMilliseconds(milliseconds)
        : null;

    private static string FileNamePart(string value)
    {
        var text = new StringBuilder();
        var pendingSeparator = false;
        foreach (var character in value.Trim().ToLowerInvariant())
        {
            if (char.IsLetterOrDigit(character))
            {
                if (pendingSeparator && text.Length > 0) text.Append('-');
                text.Append(character);
                pendingSeparator = false;
            }
            else
            {
                pendingSeparator = true;
            }
        }
        return text.Length == 0 ? "gaming" : text.ToString();
    }

    private sealed record RecordingDocument(
        string Format,
        int SchemaVersion,
        DateTimeOffset ExportedUtc,
        SessionDocument Session,
        IReadOnlyList<SampleDocument> Samples);

    private sealed record SessionDocument(
        long Id,
        string GameId,
        string GameName,
        string Objective,
        uint ProcessId,
        long ProcessCreateTime100Ns,
        DateTimeOffset? StartUtc,
        DateTimeOffset? EndUtc,
        string CaptureQuality,
        string ConfigurationSnapshotHash,
        long AppliedPlanId,
        bool Comparable,
        SummaryDocument? Summary,
        IReadOnlyList<string> Limitations)
    {
        public static SessionDocument From(GameSession session) => new(
            session.Id,
            session.GameId,
            session.GameName,
            session.Objective.ToString(),
            session.ProcessId,
            session.ProcessCreateTime100Ns,
            UnixTime(session.StartMs),
            UnixTime(session.EndMs),
            session.CaptureQuality.ToString(),
            session.ConfigurationSnapshotHash,
            session.AppliedPlanId,
            session.Comparable,
            session.Summary is null ? null : SummaryDocument.From(session.Summary),
            session.Limitations.ToArray());
    }

    private sealed record SummaryDocument(
        double AverageFps,
        double OnePercentLowFps,
        double PointOnePercentLowFps,
        double FrameTimeP50Ms,
        double FrameTimeP95Ms,
        double FrameTimeP99Ms,
        double MissedBudgetPercent,
        uint LongFrameCount,
        double CpuAveragePercent,
        double GpuAveragePercent,
        ulong VramPeakBytes,
        ulong RamPeakBytes,
        double TemperaturePeakC,
        IReadOnlyList<string> Contention,
        IReadOnlyList<string> Incidents,
        IReadOnlyList<string> Limitations)
    {
        public static SummaryDocument From(GameSessionSummary summary) => new(
            summary.AverageFps,
            summary.OnePercentLowFps,
            summary.PointOnePercentLowFps,
            summary.FrameTimeP50Ms,
            summary.FrameTimeP95Ms,
            summary.FrameTimeP99Ms,
            summary.MissedBudgetPercent,
            summary.LongFrameCount,
            summary.CpuAveragePercent,
            summary.GpuAveragePercent,
            summary.VramPeakBytes,
            summary.RamPeakBytes,
            summary.TemperaturePeakC,
            summary.Contention.ToArray(),
            summary.Incidents.ToArray(),
            summary.Limitations.ToArray());
    }

    private sealed record SampleDocument(
        DateTimeOffset? TimestampUtc,
        double FrameTimeP95Ms,
        double CpuPercent,
        double GpuPercent,
        ulong VramUsedBytes,
        ulong RamUsedBytes,
        double TemperatureC,
        ulong DiskBytesPerSec,
        uint BackgroundProcesses,
        bool Incident,
        bool DataGap,
        string EventLabel)
    {
        public static SampleDocument From(GamingTraceBucket sample) => new(
            UnixTime(sample.TsMs),
            sample.FrameTimeMs,
            sample.CpuPercent,
            sample.GpuPercent,
            sample.VramUsedBytes,
            sample.RamUsedBytes,
            sample.TemperatureC,
            sample.DiskBytesPerSec,
            sample.BackgroundProcesses,
            sample.Incident,
            sample.DataGap,
            sample.EventLabel);
    }
}
