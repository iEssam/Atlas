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
    public long TimestampMs { get; }
    public MetricsSource Source { get; }
    public double CpuPercent { get; }
    public double GpuPercent { get; }
    public ulong MemUsed { get; }
    public ulong MemTotal { get; }
    public ulong CommitUsed { get; }
    public ulong CommitLimit { get; }
    public uint ProcessCount { get; }
    public uint ThreadCount { get; }
    public uint HandleCount { get; }
    public ulong GpuDedicatedUsed { get; }
    public ulong GpuDedicatedBudget { get; }
    public ulong GpuSharedUsed { get; }
    public ulong GpuSharedBudget { get; }
    public IReadOnlyList<MetricsRow> Rows { get; }

    public MetricsSnapshot(
        long timestampMs, MetricsSource source, double cpuPercent, double gpuPercent,
        ulong memUsed, ulong memTotal,
        ulong commitUsed, ulong commitLimit, uint processCount, uint threadCount,
        uint handleCount, ulong gpuDedicatedUsed, ulong gpuDedicatedBudget,
        ulong gpuSharedUsed, ulong gpuSharedBudget, IReadOnlyList<MetricsRow> rows)
    {
        TimestampMs = timestampMs;
        Source = source;
        CpuPercent = cpuPercent;
        GpuPercent = gpuPercent;
        MemUsed = memUsed;
        MemTotal = memTotal;
        CommitUsed = commitUsed;
        CommitLimit = commitLimit;
        ProcessCount = processCount;
        ThreadCount = threadCount;
        HandleCount = handleCount;
        GpuDedicatedUsed = gpuDedicatedUsed;
        GpuDedicatedBudget = gpuDedicatedBudget;
        GpuSharedUsed = gpuSharedUsed;
        GpuSharedBudget = gpuSharedBudget;
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
    public double GpuPercent { get; }
    public ulong WorkingSet { get; }
    public ulong PrivateBytes { get; }
    public ulong ReadBps { get; }
    public ulong WriteBps { get; }
    public uint ThreadCount { get; }
    public uint HandleCount { get; }
    public ulong GpuDedicatedBytes { get; }
    public ulong GpuSharedBytes { get; }
    public string AppGroup { get; }

    public MetricsRow(
        uint pid, long createTime100ns, string imageName, double cpuPercent, double gpuPercent,
        ulong workingSet, ulong privateBytes, ulong readBps, ulong writeBps,
        uint threadCount, uint handleCount, ulong gpuDedicatedBytes, ulong gpuSharedBytes,
        string appGroup)
    {
        Pid = pid;
        CreateTime100ns = createTime100ns;
        ImageName = imageName;
        CpuPercent = cpuPercent;
        GpuPercent = gpuPercent;
        WorkingSet = workingSet;
        PrivateBytes = privateBytes;
        ReadBps = readBps;
        WriteBps = writeBps;
        ThreadCount = threadCount;
        HandleCount = handleCount;
        GpuDedicatedBytes = gpuDedicatedBytes;
        GpuSharedBytes = gpuSharedBytes;
        AppGroup = appGroup;
    }
}
