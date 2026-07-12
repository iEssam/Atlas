using System.Runtime.InteropServices;

namespace Atlas.IpcClient;

/// <summary>
/// Byte offsets and sizes of the shared-ring layout, pinned to the repr(C)
/// definitions in <c>crates/atlas-ipc/src/shm.rs</c>. Every constant below is
/// derived by hand from the Rust field order + natural alignment and asserted
/// against <c>Marshal.SizeOf</c> / <c>Marshal.OffsetOf</c> of the mirrored
/// structs in the unit tests (<c>MetricsRingLayoutTests</c>).
///
/// <para>
/// The Rust <c>RingLayout</c> is <c>{ header: RingHeader, rows: [RingRow; 64] }</c>,
/// both <c>#[repr(C)]</c>, so the section is simply the header immediately
/// followed by the row array with no extra padding (header size is already a
/// multiple of 8).
/// </para>
/// </summary>
internal static class RingLayout
{
    /// <summary>The header sits at the start of the section.</summary>
    public const long HeaderOffset = 0;

    /// <summary>
    /// The row array immediately follows the header. shm.rs RingHeader is
    /// 80 bytes (see <see cref="RingHeaderLayout.Size"/>); RingRow is 8-byte
    /// aligned so no gap is inserted between header and rows.
    /// </summary>
    public const long RowsOffset = RingHeaderLayout.Size;

    /// <summary>
    /// Full section size = header (80) + 64 rows × 104 = 80 + 6656 = 6736.
    /// shm.rs: <c>pub const RING_SIZE: usize = size_of::&lt;RingLayout&gt;();</c>
    /// </summary>
    public const long Size = RowsOffset + (long)MetricsRing.RingRows * RingRowLayout.Size;
}

/// <summary>
/// Offsets within <c>RingHeader</c> (repr(C), shm.rs). Field order and the two
/// explicit padding words are quoted from the Rust source; the u64 gauges are
/// 8-byte aligned, which <c>_pad</c> (after the AtomicU32 seq) guarantees.
///
/// <code>
/// #[repr(C)]
/// pub struct RingHeader {
///     pub magic: u32,          // @0
///     pub layout_version: u32, // @4
///     pub seq: AtomicU32,      // @8
///     _pad: u32,               // @12
///     pub ts_ms: i64,          // @16
///     pub cpu_permille: u32,   // @24
///     pub process_count: u32,  // @28
///     pub thread_count: u32,   // @32
///     pub handle_count: u32,   // @36
///     pub mem_used: u64,       // @40
///     pub mem_total: u64,      // @48
///     pub commit_used: u64,    // @56
///     pub commit_limit: u64,   // @64
///     pub row_count: u32,      // @72
///     _pad2: u32,              // @76
/// } // size = 80
/// </code>
/// </summary>
internal static class RingHeaderLayout
{
    public const long MagicOffset = 0;
    public const long LayoutVersionOffset = 4;
    public const long SeqOffset = 8;
    public const long PadOffset = 12;
    public const long TsMsOffset = 16;
    public const long CpuPermilleOffset = 24;
    public const long ProcessCountOffset = 28;
    public const long ThreadCountOffset = 32;
    public const long HandleCountOffset = 36;
    public const long MemUsedOffset = 40;
    public const long MemTotalOffset = 48;
    public const long CommitUsedOffset = 56;
    public const long CommitLimitOffset = 64;
    public const long RowCountOffset = 72;
    public const long Pad2Offset = 76;

    /// <summary>Header size in bytes (multiple of 8). shm.rs RingHeader.</summary>
    public const long Size = 80;
}

/// <summary>
/// Offsets within <c>RingRow</c> (repr(C), shm.rs). The name is a fixed
/// <c>[u16; 32]</c> = 64 bytes.
///
/// <code>
/// #[repr(C)]
/// pub struct RingRow {
///     pub pid: u32,           // @0
///     pub cpu_permille: u32,  // @4
///     pub working_set: u64,   // @8
///     pub private_bytes: u64, // @16
///     pub read_bps: u64,      // @24
///     pub write_bps: u64,     // @32
///     pub name: [u16; 32],    // @40 (64 bytes)
/// } // size = 104
/// </code>
/// </summary>
internal static class RingRowLayout
{
    public const long PidOffset = 0;
    public const long CpuPermilleOffset = 4;
    public const long WorkingSetOffset = 8;
    public const long PrivateBytesOffset = 16;
    public const long ReadBpsOffset = 24;
    public const long WriteBpsOffset = 32;
    public const long NameOffset = 40;

    /// <summary>Row size in bytes. shm.rs RingRow.</summary>
    public const long Size = 104;
}

// ---------------------------------------------------------------------------
// Mirrored blittable structs. These exist ONLY so the unit tests can assert
// Marshal.SizeOf / Marshal.OffsetOf against the offset constants above (and
// hence against the Rust repr(C) layout). The reader itself uses the explicit
// offset constants and MemoryMappedViewAccessor reads, not these structs, to
// avoid taking a hard dependency on the runtime's struct marshalling for the
// hot path.
// ---------------------------------------------------------------------------

/// <summary>
/// Blittable mirror of Rust <c>RingHeader</c> for layout assertions. The
/// AtomicU32 <c>seq</c> is a plain <c>uint</c> here (same 4 bytes, same offset).
/// </summary>
[StructLayout(LayoutKind.Sequential, Pack = 8)]
internal struct RingHeaderBlittable
{
    public uint Magic;
    public uint LayoutVersion;
    public uint Seq;
    public uint Pad;
    public long TsMs;
    public uint CpuPermille;
    public uint ProcessCount;
    public uint ThreadCount;
    public uint HandleCount;
    public ulong MemUsed;
    public ulong MemTotal;
    public ulong CommitUsed;
    public ulong CommitLimit;
    public uint RowCount;
    public uint Pad2;
}

/// <summary>Blittable mirror of Rust <c>RingRow</c> for layout assertions.</summary>
[StructLayout(LayoutKind.Sequential, Pack = 8)]
internal struct RingRowBlittable
{
    public uint Pid;
    public uint CpuPermille;
    public ulong WorkingSet;
    public ulong PrivateBytes;
    public ulong ReadBps;
    public ulong WriteBps;

    [MarshalAs(UnmanagedType.ByValArray, SizeConst = MetricsRing.RingNameLen)]
    public ushort[] Name;
}
