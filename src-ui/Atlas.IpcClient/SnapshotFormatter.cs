using System.Text;
using Atlas.V0;

namespace Atlas.IpcClient;

/// <summary>
/// Pure formatting helpers for rendering a <see cref="SnapshotReply"/> as text.
/// Mirrors the Rust dev client's <c>print_snapshot</c> / <c>format_snapshot_line</c>
/// (<c>atlas-service/src/main.rs</c>) so the C# proof reads identically. Kept
/// free of I/O so it is unit-testable.
/// </summary>
public static class SnapshotFormatter
{
    private const double BytesPerGb = 1024.0 * 1024.0 * 1024.0;
    private const double BytesPerMb = 1024.0 * 1024.0;

    /// <summary>Bytes → gibibytes.</summary>
    public static double Gb(ulong bytes) => bytes / BytesPerGb;

    /// <summary>Bytes → mebibytes.</summary>
    public static double Mb(ulong bytes) => bytes / BytesPerMb;

    /// <summary>Permille (0..1000) → percent (0..100).</summary>
    public static double Percent(uint permille) => permille / 10.0;

    /// <summary>Truncates <paramref name="s"/> to <paramref name="max"/> chars.</summary>
    public static string Truncate(string s, int max) =>
        s.Length <= max ? s : s[..max];

    /// <summary>The system gauge summary line (empty if no system gauges).</summary>
    public static string SystemLine(SnapshotReply reply)
    {
        if (reply.System is null)
        {
            return string.Empty;
        }

        var s = reply.System;
        return string.Format(
            System.Globalization.CultureInfo.InvariantCulture,
            "System: CPU {0:F1}%  Memory {1:F1}/{2:F1} GB  Commit {3:F1}/{4:F1} GB  {5} processes, {6} threads, {7} handles",
            Percent(s.CpuPermille),
            Gb(s.MemUsed), Gb(s.MemTotal),
            Gb(s.CommitUsed), Gb(s.CommitLimit),
            s.ProcessCount, s.ThreadCount, s.HandleCount);
    }

    /// <summary>The table header row (matches the Rust column layout).</summary>
    public static string HeaderRow() => string.Format(
        System.Globalization.CultureInfo.InvariantCulture,
        "{0,7} {1,-30} {2,6} {3,9} {4,9} {5,5} {6,7}",
        "PID", "NAME", "CPU%", "WS MB", "PRIV MB", "THR", "HANDLE");

    /// <summary>One formatted process row.</summary>
    public static string ProcessRowLine(ProcessRow p) => string.Format(
        System.Globalization.CultureInfo.InvariantCulture,
        "{0,7} {1,-30} {2,6:F1} {3,9:F1} {4,9:F1} {5,5} {6,7}",
        p.Pid,
        Truncate(p.ImageName, 30),
        Percent(p.CpuPermille),
        Mb(p.WorkingSet),
        Mb(p.PrivateBytes),
        p.ThreadCount,
        p.HandleCount);

    /// <summary>Full multi-line dump: system line, header, all rows.</summary>
    public static string RenderTable(SnapshotReply reply)
    {
        var sb = new StringBuilder();
        var sys = SystemLine(reply);
        if (sys.Length > 0)
        {
            sb.AppendLine(sys);
        }
        sb.AppendLine(HeaderRow());
        foreach (var p in reply.Processes)
        {
            sb.AppendLine(ProcessRowLine(p));
        }
        return sb.ToString();
    }

    /// <summary>One-line summary for <c>--watch</c> (mirrors the Rust variant).</summary>
    public static string WatchLine(SnapshotReply reply)
    {
        var cpu = reply.System is null ? 0.0 : Percent(reply.System.CpuPermille);
        var top = reply.Processes.Count > 0
            ? $"{reply.Processes[0].ImageName} {Percent(reply.Processes[0].CpuPermille):F1}%"
            : "-";
        return string.Format(
            System.Globalization.CultureInfo.InvariantCulture,
            "CPU {0,5:F1}%  procs {1,4}  top: {2}",
            cpu, reply.Processes.Count, top);
    }
}
