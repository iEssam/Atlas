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
        // shm.rs: pub const LAYOUT_VERSION: u32 = 2;
        Assert.Equal(2u, MetricsRing.LayoutVersion);
        // shm.rs: pub const RING_ROWS: usize = 64;
        Assert.Equal(64, MetricsRing.RingRows);
        // shm.rs: pub const RING_NAME_LEN: usize = 32;
        Assert.Equal(32, MetricsRing.RingNameLen);
    }

    // ---- RingHeader offsets/size -------------------------------------------

    [Fact]
    public void RingHeader_Size_Is120()
    {
        // shm.rs RingHeader v2 includes aggregate GPU utilization and memory
        // gauges. The pinned size protects older readers from silent drift.
        Assert.Equal(120, Marshal.SizeOf<RingHeaderBlittable>());
        Assert.Equal(120, RingHeaderLayout.Size);
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
    [InlineData(nameof(RingHeaderBlittable.GpuPermille), 40)]
    [InlineData(nameof(RingHeaderBlittable.GpuPad), 44)]
    [InlineData(nameof(RingHeaderBlittable.MemUsed), 48)]
    [InlineData(nameof(RingHeaderBlittable.MemTotal), 56)]
    [InlineData(nameof(RingHeaderBlittable.CommitUsed), 64)]
    [InlineData(nameof(RingHeaderBlittable.CommitLimit), 72)]
    [InlineData(nameof(RingHeaderBlittable.GpuDedicatedUsed), 80)]
    [InlineData(nameof(RingHeaderBlittable.GpuDedicatedBudget), 88)]
    [InlineData(nameof(RingHeaderBlittable.GpuSharedUsed), 96)]
    [InlineData(nameof(RingHeaderBlittable.GpuSharedBudget), 104)]
    [InlineData(nameof(RingHeaderBlittable.RowCount), 112)]
    [InlineData(nameof(RingHeaderBlittable.Pad2), 116)]
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
        Assert.Equal((long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.GpuPermille)), RingHeaderLayout.GpuPermilleOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.MemUsed)), RingHeaderLayout.MemUsedOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.MemTotal)), RingHeaderLayout.MemTotalOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.CommitUsed)), RingHeaderLayout.CommitUsedOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.CommitLimit)), RingHeaderLayout.CommitLimitOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.GpuDedicatedUsed)), RingHeaderLayout.GpuDedicatedUsedOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.GpuDedicatedBudget)), RingHeaderLayout.GpuDedicatedBudgetOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.GpuSharedUsed)), RingHeaderLayout.GpuSharedUsedOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.GpuSharedBudget)), RingHeaderLayout.GpuSharedBudgetOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.RowCount)), RingHeaderLayout.RowCountOffset);
    }

    // ---- RingRow offsets/size ----------------------------------------------

    [Fact]
    public void RingRow_Size_Is128()
    {
        // shm.rs RingRow v2 includes per-process GPU utilization and memory.
        Assert.Equal(128, Marshal.SizeOf<RingRowBlittable>());
        Assert.Equal(128, RingRowLayout.Size);
    }

    [Theory]
    [InlineData(nameof(RingRowBlittable.Pid), 0)]
    [InlineData(nameof(RingRowBlittable.CpuPermille), 4)]
    [InlineData(nameof(RingRowBlittable.GpuPermille), 8)]
    [InlineData(nameof(RingRowBlittable.PadGpu), 12)]
    [InlineData(nameof(RingRowBlittable.WorkingSet), 16)]
    [InlineData(nameof(RingRowBlittable.PrivateBytes), 24)]
    [InlineData(nameof(RingRowBlittable.ReadBps), 32)]
    [InlineData(nameof(RingRowBlittable.WriteBps), 40)]
    [InlineData(nameof(RingRowBlittable.GpuDedicatedBytes), 48)]
    [InlineData(nameof(RingRowBlittable.GpuSharedBytes), 56)]
    [InlineData(nameof(RingRowBlittable.Name), 64)]
    public void RingRow_FieldOffsets_MatchRust(string field, int expected)
    {
        Assert.Equal(expected, (int)Marshal.OffsetOf<RingRowBlittable>(field));
    }

    [Fact]
    public void RingRow_OffsetConstants_MatchMarshal()
    {
        Assert.Equal((long)Marshal.OffsetOf<RingRowBlittable>(nameof(RingRowBlittable.Pid)), RingRowLayout.PidOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingRowBlittable>(nameof(RingRowBlittable.CpuPermille)), RingRowLayout.CpuPermilleOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingRowBlittable>(nameof(RingRowBlittable.GpuPermille)), RingRowLayout.GpuPermilleOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingRowBlittable>(nameof(RingRowBlittable.WorkingSet)), RingRowLayout.WorkingSetOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingRowBlittable>(nameof(RingRowBlittable.PrivateBytes)), RingRowLayout.PrivateBytesOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingRowBlittable>(nameof(RingRowBlittable.ReadBps)), RingRowLayout.ReadBpsOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingRowBlittable>(nameof(RingRowBlittable.WriteBps)), RingRowLayout.WriteBpsOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingRowBlittable>(nameof(RingRowBlittable.GpuDedicatedBytes)), RingRowLayout.GpuDedicatedBytesOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingRowBlittable>(nameof(RingRowBlittable.GpuSharedBytes)), RingRowLayout.GpuSharedBytesOffset);
        Assert.Equal((long)Marshal.OffsetOf<RingRowBlittable>(nameof(RingRowBlittable.Name)), RingRowLayout.NameOffset);
    }

    // ---- Whole-section size ------------------------------------------------

    [Fact]
    public void RingLayout_Size_Is8312()
    {
        // shm.rs RING_SIZE = header(120) + 64*row(128) = 8312.
        Assert.Equal(8312, RingLayout.Size);
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
