using System.IO.MemoryMappedFiles;
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using System.Threading;
using Microsoft.Win32.SafeHandles;

namespace Atlas.IpcClient;

/// <summary>
/// Read-only reader for the Atlas shared-memory live ring — the lock-free hot
/// path that mirrors the Rust writer in <c>crates/atlas-ipc/src/shm.rs</c>. The
/// service (<c>serve</c>) publishes a fixed-size, pagefile-backed named section
/// (<c>Local\SystemAtlas.metrics.&lt;disc&gt;</c>) once per second: the system
/// gauges plus the top-N process rows. Unprivileged readers map it read-only and
/// copy out a consistent snapshot under a seqlock with zero writer contention.
///
/// <para>
/// The seqlock protocol matches the Rust side exactly (see shm.rs module docs):
/// the header holds a sequence counter that is <b>even when stable, odd while a
/// write is in progress</b>. A reader <see cref="Volatile.Read"/>s the sequence
/// (retrying while odd), copies the payload with plain reads, issues a memory
/// barrier, re-reads the sequence, and retries on any change. After a bounded
/// number of retries it gives up rather than spin forever (a writer stuck mid-
/// publish). Magic + layout version are validated on open; a version mismatch is
/// reported distinctly rather than parsed as garbage.
/// </para>
///
/// <para>
/// The section is mapped read-only and a stable base pointer is pinned for the
/// reader's lifetime (<see cref="SafeMemoryMappedViewHandle.AcquirePointer"/>),
/// so the hot path reads fixed offsets directly with no per-read handle
/// bookkeeping. All layout offsets live in <see cref="RingLayout"/> and are
/// asserted against the Rust repr(C) sizes in the unit tests.
/// </para>
/// </summary>
[System.Runtime.Versioning.SupportedOSPlatform("windows")]
public sealed unsafe class MetricsRing : IDisposable
{
    // ---- Constants pinned to crates/atlas-ipc/src/shm.rs -------------------

    /// <summary>
    /// Section magic — "ALSR" (AtLas Shared Ring), little-endian.
    /// shm.rs: <c>pub const RING_MAGIC: u32 = 0x414C_5352;</c>
    /// </summary>
    public const uint RingMagic = 0x414C_5352;

    /// <summary>
    /// Supported layout version. Bumped by the Rust side on any layout change so
    /// a stale reader rejects an incompatible section.
    /// shm.rs: <c>pub const LAYOUT_VERSION: u32 = 1;</c>
    /// </summary>
    public const uint LayoutVersion = 2;

    /// <summary>
    /// Fixed number of process rows in the ring.
    /// shm.rs: <c>pub const RING_ROWS: usize = 64;</c>
    /// </summary>
    public const int RingRows = 64;

    /// <summary>
    /// Fixed capacity (UTF-16 code units) of a row's NUL-padded image name.
    /// shm.rs: <c>pub const RING_NAME_LEN: usize = 32;</c>
    /// </summary>
    public const int RingNameLen = 32;

    /// <summary>
    /// Bounded seqlock retry budget, mirroring shm.rs
    /// <c>const SNAPSHOT_RETRIES: u32 = 1024;</c>. A live 1 Hz writer never
    /// collides for long; the cap only guards a writer stuck with an odd seq.
    /// </summary>
    private const int SnapshotRetries = 1024;

    private readonly MemoryMappedFile _mmf;
    private readonly MemoryMappedViewAccessor _view;
    private readonly SafeMemoryMappedViewHandle _handle;
    private readonly byte* _base;
    private bool _disposed;

    private MetricsRing(MemoryMappedFile mmf, MemoryMappedViewAccessor view)
    {
        _mmf = mmf;
        _view = view;
        _handle = view.SafeMemoryMappedViewHandle;

        // Pin a stable base pointer for the reader's lifetime. The accessor may
        // begin partway into the first page; PointerOffset is that intra-view
        // delta (0 here since we map from offset 0, but honoured for safety).
        byte* ptr = null;
        _handle.AcquirePointer(ref ptr);
        _base = ptr + view.PointerOffset;
    }

    /// <summary>
    /// Builds the section object name for a discriminator, mirroring the Rust
    /// <c>section_name</c>: <c>Local\SystemAtlas.metrics.&lt;who&gt;</c>. The
    /// <c>Local\</c> prefix scopes it to the caller's session.
    /// </summary>
    public static string SectionName(string who) => $@"Local\SystemAtlas.metrics.{who}";

    /// <summary>
    /// Resolves the ring discriminator the same way the Rust
    /// <c>ring_discriminator</c> does: the supplied token, else the
    /// <c>USERNAME</c> env var, falling back to <c>session</c>. This matches the
    /// pipe discriminator so ring and gRPC rendezvous on one token.
    /// </summary>
    public static string DefaultWho() => AtlasPipe.DefaultWho();

    /// <summary>
    /// Tries to open the existing named section for <paramref name="who"/>
    /// read-only and validate its magic + layout version. Returns a result that
    /// distinguishes the three outcomes cleanly:
    /// <list type="bullet">
    /// <item><see cref="RingOpenStatus.Opened"/> — mapped and valid.</item>
    /// <item><see cref="RingOpenStatus.NotFound"/> — no section (writer not
    ///   running / wrong discriminator).</item>
    /// <item><see cref="RingOpenStatus.Incompatible"/> — magic or version
    ///   mismatch (a foreign or stale section); never treated as garbage.</item>
    /// </list>
    /// </summary>
    public static RingOpenResult TryOpen(string? who = null)
    {
        var disc = string.IsNullOrEmpty(who) ? DefaultWho() : who;
        var name = SectionName(disc);

        MemoryMappedFile? mmf = null;
        MemoryMappedViewAccessor? view = null;
        MetricsRing? ring = null;
        try
        {
            // Read access only — the reader never writes the section.
            mmf = MemoryMappedFile.OpenExisting(name, MemoryMappedFileRights.Read);
            view = mmf.CreateViewAccessor(0, RingLayout.Size, MemoryMappedFileAccess.Read);

            ring = new MetricsRing(mmf, view);

            // Validate magic/version before trusting any payload. These header
            // fields never change after the writer's init, so no seqlock is
            // needed for this check (matches the Rust `validate`).
            uint magic = ring.ReadU32(RingLayout.HeaderOffset + RingHeaderLayout.MagicOffset);
            uint version = ring.ReadU32(RingLayout.HeaderOffset + RingHeaderLayout.LayoutVersionOffset);

            if (magic != RingMagic)
            {
                ring.Dispose();
                return RingOpenResult.Incompatible(
                    $"ring magic mismatch: got 0x{magic:X8}, want 0x{RingMagic:X8}");
            }
            if (version != LayoutVersion)
            {
                ring.Dispose();
                return RingOpenResult.Incompatible(
                    $"ring layout version {version} != supported {LayoutVersion}");
            }

            return RingOpenResult.Success(ring);
        }
        catch (FileNotFoundException)
        {
            // Section does not exist — writer not started, or wrong discriminator.
            ring?.Dispose();
            view?.Dispose();
            mmf?.Dispose();
            return RingOpenResult.NotFound($"section '{name}' not found");
        }
        catch (Exception ex)
        {
            ring?.Dispose();
            view?.Dispose();
            mmf?.Dispose();
            return RingOpenResult.NotFound($"open '{name}' failed: {ex.Message}");
        }
    }

    /// <summary>
    /// Copies out one consistent snapshot under the seqlock. Returns
    /// <c>null</c> only when the writer is mid-publish for the entire retry
    /// budget (writer stalled with an odd seq) — a correct "try again" outcome,
    /// never a torn read.
    /// </summary>
    public RingSnapshot? Snapshot()
    {
        ObjectDisposedException.ThrowIf(_disposed, this);

        ref int seq = ref Unsafe.AsRef<int>(
            _base + RingLayout.HeaderOffset + RingHeaderLayout.SeqOffset);

        for (int attempt = 0; attempt < SnapshotRetries; attempt++)
        {
            // Acquire-load the sequence; odd = a write is in progress.
            // Volatile.Read gives the acquire semantics that pair with the
            // writer's Release store of the opening (odd) increment.
            uint s1 = (uint)Volatile.Read(ref seq);
            if ((s1 & 1) != 0)
            {
                Thread.SpinWait(1);
                continue;
            }

            // Copy the payload with plain reads. Trusting these bytes is sound
            // only because the seq re-check below discards a copy that raced a
            // publish (identical reasoning to the Rust reader).
            var snap = CopyOut();

            // Order the payload reads ahead of the second seq load — the
            // portable full barrier standing in for the Rust reader's
            // fence(Acquire).
            Interlocked.MemoryBarrier();

            uint s2 = (uint)Volatile.Read(ref seq);
            if (s1 == s2)
            {
                // Even and unchanged: exactly one publish's output.
                return snap;
            }
            // A publish overlapped the copy; retry.
            Thread.SpinWait(1);
        }

        return null;
    }

    /// <summary>
    /// Reads the gauges and the first <c>row_count</c> rows into an owned
    /// snapshot. The caller holds the seqlock invariant around this.
    /// </summary>
    private RingSnapshot CopyOut()
    {
        long hOff = RingLayout.HeaderOffset;

        long tsMs = ReadI64(hOff + RingHeaderLayout.TsMsOffset);
        uint cpu = ReadU32(hOff + RingHeaderLayout.CpuPermilleOffset);
        uint procCount = ReadU32(hOff + RingHeaderLayout.ProcessCountOffset);
        uint threadCount = ReadU32(hOff + RingHeaderLayout.ThreadCountOffset);
        uint handleCount = ReadU32(hOff + RingHeaderLayout.HandleCountOffset);
        uint gpu = ReadU32(hOff + RingHeaderLayout.GpuPermilleOffset);
        ulong memUsed = ReadU64(hOff + RingHeaderLayout.MemUsedOffset);
        ulong memTotal = ReadU64(hOff + RingHeaderLayout.MemTotalOffset);
        ulong commitUsed = ReadU64(hOff + RingHeaderLayout.CommitUsedOffset);
        ulong commitLimit = ReadU64(hOff + RingHeaderLayout.CommitLimitOffset);
        ulong gpuDedicatedUsed = ReadU64(hOff + RingHeaderLayout.GpuDedicatedUsedOffset);
        ulong gpuDedicatedBudget = ReadU64(hOff + RingHeaderLayout.GpuDedicatedBudgetOffset);
        ulong gpuSharedUsed = ReadU64(hOff + RingHeaderLayout.GpuSharedUsedOffset);
        ulong gpuSharedBudget = ReadU64(hOff + RingHeaderLayout.GpuSharedBudgetOffset);
        uint rowCount = ReadU32(hOff + RingHeaderLayout.RowCountOffset);

        int n = (int)Math.Min(rowCount, (uint)RingRows);
        var rows = new List<RingRowSnapshot>(n);
        for (int i = 0; i < n; i++)
        {
            long rowBase = RingLayout.RowsOffset + (long)i * RingRowLayout.Size;
            uint pid = ReadU32(rowBase + RingRowLayout.PidOffset);
            uint rcpu = ReadU32(rowBase + RingRowLayout.CpuPermilleOffset);
            uint rgpu = ReadU32(rowBase + RingRowLayout.GpuPermilleOffset);
            ulong ws = ReadU64(rowBase + RingRowLayout.WorkingSetOffset);
            ulong priv = ReadU64(rowBase + RingRowLayout.PrivateBytesOffset);
            ulong readBps = ReadU64(rowBase + RingRowLayout.ReadBpsOffset);
            ulong writeBps = ReadU64(rowBase + RingRowLayout.WriteBpsOffset);
            ulong gpuDedicated = ReadU64(rowBase + RingRowLayout.GpuDedicatedBytesOffset);
            ulong gpuShared = ReadU64(rowBase + RingRowLayout.GpuSharedBytesOffset);
            string name = ReadName(rowBase + RingRowLayout.NameOffset);

            rows.Add(new RingRowSnapshot(pid, rcpu, rgpu, ws, priv, readBps, writeBps,
                gpuDedicated, gpuShared, name));
        }

        return new RingSnapshot(
            tsMs, cpu, gpu, procCount, threadCount, handleCount,
            memUsed, memTotal, commitUsed, commitLimit,
            gpuDedicatedUsed, gpuDedicatedBudget, gpuSharedUsed, gpuSharedBudget, rows);
    }

    /// <summary>
    /// Decodes a NUL-padded, truncated UTF-16 image name from the row, stopping
    /// at the first NUL (mirrors <c>RingRow::name_string</c>).
    /// </summary>
    private string ReadName(long offset)
    {
        Span<char> buf = stackalloc char[RingNameLen];
        int len = 0;
        for (int u = 0; u < RingNameLen; u++)
        {
            ushort unit = ReadU16(offset + (long)u * sizeof(ushort));
            if (unit == 0)
            {
                break;
            }
            buf[len++] = (char)unit;
        }
        return new string(buf.Slice(0, len));
    }

    // Little-endian, unaligned-safe reads at a byte offset into the mapped view.
    // Unsafe.ReadUnaligned matches the x86/x64/arm64 tolerance for the (already
    // naturally-aligned) repr(C) fields and never faults on the fixed layout.
    private uint ReadU32(long offset) => Unsafe.ReadUnaligned<uint>(_base + offset);
    private ulong ReadU64(long offset) => Unsafe.ReadUnaligned<ulong>(_base + offset);
    private long ReadI64(long offset) => Unsafe.ReadUnaligned<long>(_base + offset);
    private ushort ReadU16(long offset) => Unsafe.ReadUnaligned<ushort>(_base + offset);

    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }
        _disposed = true;
        if (_base != null)
        {
            _handle.ReleasePointer();
        }
        _view.Dispose();
        _mmf.Dispose();
    }
}
