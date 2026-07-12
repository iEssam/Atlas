using System.Collections.Generic;

namespace Atlas.App.Services;

/// <summary>
/// Where a <see cref="MetricsSnapshot"/> came from. Surfaced subtly in the UI
/// status line so a viewer can tell the hot path (shared-memory ring) from the
/// gRPC stream fallback.
/// </summary>
public enum MetricsSource
{
    /// <summary>Not yet connected to either source.</summary>
    None,

    /// <summary>The shared-memory live ring (the preferred, lock-free hot path).</summary>
    Ring,

    /// <summary>The gRPC <c>StreamSnapshots</c> fallback.</summary>
    Stream,
}

/// <summary>
/// A source-agnostic snapshot the pages bind to, normalized from either the
/// shared-memory ring (<see cref="Atlas.IpcClient.RingSnapshot"/>) or a gRPC
/// <c>SnapshotReply</c>. Values are the measured gauges plus the top process
/// rows; no derived "health" claims (PRD §9.1).
/// </summary>
public sealed class MetricsSnapshot
{
    public MetricsSource Source { get; }
    public double CpuPercent { get; }
    public ulong MemUsed { get; }
    public ulong MemTotal { get; }
    public ulong CommitUsed { get; }
    public ulong CommitLimit { get; }
    public uint ProcessCount { get; }
    public uint ThreadCount { get; }
    public uint HandleCount { get; }
    public IReadOnlyList<MetricsRow> Rows { get; }

    public MetricsSnapshot(
        MetricsSource source, double cpuPercent, ulong memUsed, ulong memTotal,
        ulong commitUsed, ulong commitLimit, uint processCount, uint threadCount,
        uint handleCount, IReadOnlyList<MetricsRow> rows)
    {
        Source = source;
        CpuPercent = cpuPercent;
        MemUsed = memUsed;
        MemTotal = memTotal;
        CommitUsed = commitUsed;
        CommitLimit = commitLimit;
        ProcessCount = processCount;
        ThreadCount = threadCount;
        HandleCount = handleCount;
        Rows = rows;
    }
}

/// <summary>
/// One normalized process row. Identity is <see cref="Pid"/> plus
/// <see cref="CreateTime100ns"/>; the ring carries only the pid (create-time is
/// 0 there), while the gRPC path carries both. Rows are already CPU-desc sorted
/// by the server/writer.
/// </summary>
public sealed class MetricsRow
{
    public uint Pid { get; }
    public long CreateTime100ns { get; }
    public string ImageName { get; }
    public double CpuPercent { get; }
    public ulong WorkingSet { get; }
    public ulong PrivateBytes { get; }
    public uint ThreadCount { get; }
    public uint HandleCount { get; }

    public MetricsRow(
        uint pid, long createTime100ns, string imageName, double cpuPercent,
        ulong workingSet, ulong privateBytes, uint threadCount, uint handleCount)
    {
        Pid = pid;
        CreateTime100ns = createTime100ns;
        ImageName = imageName;
        CpuPercent = cpuPercent;
        WorkingSet = workingSet;
        PrivateBytes = privateBytes;
        ThreadCount = threadCount;
        HandleCount = handleCount;
    }
}
