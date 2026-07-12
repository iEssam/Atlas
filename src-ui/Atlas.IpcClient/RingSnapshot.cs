namespace Atlas.IpcClient;

/// <summary>
/// Outcome of <see cref="MetricsRing.TryOpen"/>, distinguishing "no section",
/// "incompatible section", and "opened" so the UI can fall back to gRPC without
/// ever mistaking a foreign/stale section for live data.
/// </summary>
public enum RingOpenStatus
{
    /// <summary>The section was mapped and its magic + version validated.</summary>
    Opened,

    /// <summary>No section exists (writer not running, or wrong discriminator).</summary>
    NotFound,

    /// <summary>The section exists but its magic or layout version is unsupported.</summary>
    Incompatible,
}

/// <summary>
/// Result of trying to open the ring. On <see cref="RingOpenStatus.Opened"/>,
/// <see cref="Ring"/> is non-null and owned by the caller (dispose it).
/// Otherwise <see cref="Message"/> explains why.
/// </summary>
public sealed class RingOpenResult
{
    public RingOpenStatus Status { get; }
    public MetricsRing? Ring { get; }
    public string Message { get; }

    private RingOpenResult(RingOpenStatus status, MetricsRing? ring, string message)
    {
        Status = status;
        Ring = ring;
        Message = message;
    }

    public bool IsOpened => Status == RingOpenStatus.Opened && Ring is not null;

    internal static RingOpenResult Success(MetricsRing ring) =>
        new(RingOpenStatus.Opened, ring, "opened");

    internal static RingOpenResult NotFound(string message) =>
        new(RingOpenStatus.NotFound, null, message);

    internal static RingOpenResult Incompatible(string message) =>
        new(RingOpenStatus.Incompatible, null, message);
}

/// <summary>
/// A consistent, owned copy of the ring at one instant: the system gauges plus
/// the live process rows (already truncated to <c>row_count</c>). Mirrors the
/// Rust <c>RingSnapshot</c>. Carries no atomics and no view references.
/// </summary>
public sealed class RingSnapshot
{
    public long TsMs { get; }
    public uint CpuPermille { get; }
    public uint ProcessCount { get; }
    public uint ThreadCount { get; }
    public uint HandleCount { get; }
    public ulong MemUsed { get; }
    public ulong MemTotal { get; }
    public ulong CommitUsed { get; }
    public ulong CommitLimit { get; }

    /// <summary>Valid process rows, top-N sorted CPU-desc by the writer.</summary>
    public IReadOnlyList<RingRowSnapshot> Rows { get; }

    public RingSnapshot(
        long tsMs, uint cpuPermille, uint processCount, uint threadCount,
        uint handleCount, ulong memUsed, ulong memTotal, ulong commitUsed,
        ulong commitLimit, IReadOnlyList<RingRowSnapshot> rows)
    {
        TsMs = tsMs;
        CpuPermille = cpuPermille;
        ProcessCount = processCount;
        ThreadCount = threadCount;
        HandleCount = handleCount;
        MemUsed = memUsed;
        MemTotal = memTotal;
        CommitUsed = commitUsed;
        CommitLimit = commitLimit;
        Rows = rows;
    }
}

/// <summary>One copied-out process row (owns its decoded name).</summary>
public sealed class RingRowSnapshot
{
    public uint Pid { get; }
    public uint CpuPermille { get; }
    public ulong WorkingSet { get; }
    public ulong PrivateBytes { get; }
    public ulong ReadBps { get; }
    public ulong WriteBps { get; }
    public string Name { get; }

    public RingRowSnapshot(
        uint pid, uint cpuPermille, ulong workingSet, ulong privateBytes,
        ulong readBps, ulong writeBps, string name)
    {
        Pid = pid;
        CpuPermille = cpuPermille;
        WorkingSet = workingSet;
        PrivateBytes = privateBytes;
        ReadBps = readBps;
        WriteBps = writeBps;
        Name = name;
    }
}
