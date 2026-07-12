using System.Runtime.InteropServices;
using Atlas.IpcClient;
using Xunit;

namespace Atlas.IpcClient.Tests;

/// <summary>
/// Pins the C# ring layout to the Rust repr(C) definitions in
/// <c>crates/atlas-ipc/src/shm.rs</c>. Every offset/size the reader relies on is
/// asserted here against the mirrored blittable structs' Marshal metadata AND
/// against the raw constants quoted from the Rust source, so a drift on either
/// side is caught at test time rather than as a torn read at runtime.
/// </summary>
public class MetricsRingLayoutTests
{
    // ---- Constants quoted from shm.rs --------------------------------------

    [Fact]
    public void Constants_MatchRustSource()
    {
        // shm.rs: pub const RING_MAGIC: u32 = 0x414C_5352; // "ALSR" LE
        Assert.Equal(0x414C_5352u, MetricsRing.RingMagic);
        // shm.rs: pub const LAYOUT_VERSION: u32 = 1;
        Assert.Equal(1u, MetricsRing.LayoutVersion);
        // shm.rs: pub const RING_ROWS: usize = 64;
        Assert.Equal(64, MetricsRing.RingRows);
        // shm.rs: pub const RING_NAME_LEN: usize = 32;
        Assert.Equal(32, MetricsRing.RingNameLen);
    }

    // ---- RingHeader offsets/size -------------------------------------------

    [Fact]
    public void RingHeader_Size_Is80()
    {
        // shm.rs RingHeader: u32 magic, u32 version, AtomicU32 seq, u32 _pad,
        // i64 ts_ms, 4×u32 gauges, 4×u64 mem/commit, u32 row_count, u32 _pad2.
        // = 16 + 8 + 16 + 32 + 8 = 80 bytes.
        Assert.Equal(80, Marshal.SizeOf<RingHeaderBlittable>());
        Assert.Equal(80, RingHeaderLayout.Size);
    }

    [Theory]
    [InlineData(nameof(RingHeaderBlittable.Magic), 0)]
    [InlineData(nameof(RingHeaderBlittable.LayoutVersion), 4)]
    [InlineData(nameof(RingHeaderBlittable.Seq), 8)]
    [InlineData(nameof(RingHeaderBlittable.Pad), 12)]
    [InlineData(nameof(RingHeaderBlittable.TsMs), 16)]
    [InlineData(nameof(RingHeaderBlittable.CpuPermille), 24)]
    [InlineData(nameof(RingHeaderBlittable.ProcessCount), 28)]
    [InlineData(nameof(RingHeaderBlittable.ThreadCount), 32)]
    [InlineData(nameof(RingHeaderBlittable.HandleCount), 36)]
    [InlineData(nameof(RingHeaderBlittable.MemUsed), 40)]
    [InlineData(nameof(RingHeaderBlittable.MemTotal), 48)]
    [InlineData(nameof(RingHeaderBlittable.CommitUsed), 56)]
    [InlineData(nameof(RingHeaderBlittable.CommitLimit), 64)]
    [InlineData(nameof(RingHeaderBlittable.RowCount), 72)]
    [InlineData(nameof(RingHeaderBlittable.Pad2), 76)]
    public void RingHeader_FieldOffsets_MatchRust(string field, int expected)
    {
        Assert.Equal(expected, (int)Marshal.OffsetOf<RingHeaderBlittable>(field));
    }

    [Fact]
    public void RingHeader_OffsetConstants_MatchMarshal()
    {
        // The reader uses the RingHeaderLayout constants directly; assert they
        // equal the marshalled struct offsets (both derived from shm.rs).
        Assert.Equal((long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.Magic)), RingHeaderLayout.MagicOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.LayoutVersion)), RingHeaderLayout.LayoutVersionOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.Seq)), RingHeaderLayout.SeqOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.TsMs)), RingHeaderLayout.TsMsOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.CpuPermille)), RingHeaderLayout.CpuPermilleOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.ProcessCount)), RingHeaderLayout.ProcessCountOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.ThreadCount)), RingHeaderLayout.ThreadCountOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.HandleCount)), RingHeaderLayout.HandleCountOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.MemUsed)), RingHeaderLayout.MemUsedOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.MemTotal)), RingHeaderLayout.MemTotalOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.CommitUsed)), RingHeaderLayout.CommitUsedOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.CommitLimit)), RingHeaderLayout.CommitLimitOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.RowCount)), RingHeaderLayout.RowCountOffset);
    }

    // ---- RingRow offsets/size ----------------------------------------------

    [Fact]
    public void RingRow_Size_Is104()
    {
        // shm.rs RingRow: u32 pid, u32 cpu, 4×u64, [u16;32] name.
        // = 8 + 32 + 64 = 104 bytes.
        Assert.Equal(104, Marshal.SizeOf<RingRowBlittable>());
        Assert.Equal(104, RingRowLayout.Size);
    }

    [Theory]
    [InlineData(nameof(RingRowBlittable.Pid), 0)]
    [InlineData(nameof(RingRowBlittable.CpuPermille), 4)]
    [InlineData(nameof(RingRowBlittable.WorkingSet), 8)]
    [InlineData(nameof(RingRowBlittable.PrivateBytes), 16)]
    [InlineData(nameof(RingRowBlittable.ReadBps), 24)]
    [InlineData(nameof(RingRowBlittable.WriteBps), 32)]
    [InlineData(nameof(RingRowBlittable.Name), 40)]
    public void RingRow_FieldOffsets_MatchRust(string field, int expected)
    {
        Assert.Equal(expected, (int)Marshal.OffsetOf<RingRowBlittable>(field));
    }

    [Fact]
    public void RingRow_OffsetConstants_MatchMarshal()
    {
        Assert.Equal((long)Marshal.OffsetOf<RingRowBlittable>(nameof(RingRowBlittable.Pid)), RingRowLayout.PidOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingRowBlittable>(nameof(RingRowBlittable.CpuPermille)), RingRowLayout.CpuPermilleOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingRowBlittable>(nameof(RingRowBlittable.WorkingSet)), RingRowLayout.WorkingSetOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingRowBlittable>(nameof(RingRowBlittable.PrivateBytes)), RingRowLayout.PrivateBytesOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingRowBlittable>(nameof(RingRowBlittable.ReadBps)), RingRowLayout.ReadBpsOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingRowBlittable>(nameof(RingRowBlittable.WriteBps)), RingRowLayout.WriteBpsOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingRowBlittable>(nameof(RingRowBlittable.Name)), RingRowLayout.NameOffset);
    }

    // ---- Whole-section size ------------------------------------------------

    [Fact]
    public void RingLayout_Size_Is6736()
    {
        // shm.rs RING_SIZE = size_of::<RingLayout>() = header(80) + 64*104 = 6736.
        Assert.Equal(6736, RingLayout.Size);
        Assert.Equal(RingHeaderLayout.Size + MetricsRing.RingRows * RingRowLayout.Size, RingLayout.Size);
    }

    [Fact]
    public void SectionName_MatchesRustScheme()
    {
        // shm.rs section_name("abc") == r"Local\SystemAtlas.metrics.abc"
        Assert.Equal(@"Local\SystemAtlas.metrics.abc", MetricsRing.SectionName("abc"));
        Assert.Equal(@"Local\SystemAtlas.metrics.uidev2", MetricsRing.SectionName("uidev2"));
    }
}
