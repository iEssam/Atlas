using Atlas.V0;

namespace Atlas.IpcClient;

/// <summary>
/// Pure presentation mappings for deterministic insights. The service owns the
/// conclusion; this class only turns enum values into calm, accessible labels.
/// </summary>
public static class InsightFormatter
{
    public static string StatusLabel(InsightStatus status) => status switch
    {
        InsightStatus.Active => "Needs attention",
        InsightStatus.Emerging => "Watch",
        InsightStatus.Resolved => "Resolved",
        InsightStatus.Clear => "Clear",
        InsightStatus.Limited => "Limited data",
        _ => "Status unknown",
    };

    public static string KindGlyph(InsightKind kind) => kind switch
    {
        InsightKind.CpuPressure => "\uE9D9",
        InsightKind.MemoryPressure => "\uE7F8",
        InsightKind.GpuMemoryPressure => "\uE7F8",
        InsightKind.GpuThermalLimit => "\uE7E8",
        InsightKind.ResourceStateClear => "\uE73E",
        _ => "\uE946",
    };

    public static string ActionLabel(string? destination)
    {
        if (string.IsNullOrWhiteSpace(destination))
        {
            return string.Empty;
        }

        if (destination.StartsWith("process:", StringComparison.Ordinal))
        {
            return "Inspect process";
        }

        return destination switch
        {
            "activity" => "Open Live Activity",
            "graphics" => "Open Graphics",
            _ => "Open evidence",
        };
    }

    public static bool TryParseProcessDestination(
        string? destination,
        out uint pid,
        out long createTime100ns,
        out string imageName)
    {
        pid = 0;
        createTime100ns = 0;
        imageName = string.Empty;
        if (string.IsNullOrWhiteSpace(destination) ||
            !destination.StartsWith("process:", StringComparison.Ordinal))
        {
            return false;
        }

        var parts = destination.Split(':', 4);
        if (parts.Length < 3 ||
            !uint.TryParse(parts[1], out pid) ||
            !long.TryParse(parts[2], out createTime100ns))
        {
            return false;
        }

        imageName = parts.Length == 4 ? parts[3] : string.Empty;
        return pid > 0;
    }

    public static string EvidenceSummary(Insight insight)
    {
        if (insight.Evidence.Count == 0)
        {
            return "No measurement is available yet.";
        }

        var count = insight.Evidence.Count;
        var prefix = $"Based on {count} measurement{(count == 1 ? string.Empty : "s")}.";
        return $"{prefix} {insight.Evidence[0].Text}";
    }
}
