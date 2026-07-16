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
        Assert.Equal(RingHeaderLayout.MagicOffset, (long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.Magic)));
        Assert.Equal(RingHeaderLayout.LayoutVersionOffset, (long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.LayoutVersion)));
        Assert.Equal(RingHeaderLayout.SeqOffset, (long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.Seq)));
        Assert.Equal(RingHeaderLayout.TsMsOffset, (long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.TsMs)));
        Assert.Equal(RingHeaderLayout.CpuPermilleOffset, (long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.CpuPermille)));
        Assert.Equal(RingHeaderLayout.ProcessCountOffset, (long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.ProcessCount)));
        Assert.Equal(RingHeaderLayout.ThreadCountOffset, (long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.ThreadCount)));
        Assert.Equal(RingHeaderLayout.HandleCountOffset, (long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.HandleCount)));
        Assert.Equal(RingHeaderLayout.GpuPermilleOffset, (long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.GpuPermille)));
        Assert.Equal(RingHeaderLayout.MemUsedOffset, (long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.MemUsed)));
        Assert.Equal(RingHeaderLayout.MemTotalOffset, (long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.MemTotal)));
        Assert.Equal(RingHeaderLayout.CommitUsedOffset, (long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.CommitUsed)));
        Assert.Equal(RingHeaderLayout.CommitLimitOffset, (long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.CommitLimit)));
        Assert.Equal(RingHeaderLayout.GpuDedicatedUsedOffset, (long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.GpuDedicatedUsed)));
        Assert.Equal(RingHeaderLayout.GpuDedicatedBudgetOffset, (long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.GpuDedicatedBudget)));
        Assert.Equal(RingHeaderLayout.GpuSharedUsedOffset, (long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.GpuSharedUsed)));
        Assert.Equal(RingHeaderLayout.GpuSharedBudgetOffset, (long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.GpuSharedBudget)));
        Assert.Equal(RingHeaderLayout.RowCountOffset, (long)Marshal.OffsetOf<RingHeaderBlittable>(nameof(RingHeaderBlittable.RowCount)));
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
        Assert.Equal(RingRowLayout.PidOffset, (long)Marshal.OffsetOf<RingRowBlittable>(nameof(RingRowBlittable.Pid)));
        Assert.Equal(RingRowLayout.CpuPermilleOffset, (long)Marshal.OffsetOf<RingRowBlittable>(nameof(RingRowBlittable.CpuPermille)));
        Assert.Equal(RingRowLayout.GpuPermilleOffset, (long)Marshal.OffsetOf<RingRowBlittable>(nameof(RingRowBlittable.GpuPermille)));
        Assert.Equal(RingRowLayout.WorkingSetOffset, (long)Marshal.OffsetOf<RingRowBlittable>(nameof(RingRowBlittable.WorkingSet)));
        Assert.Equal(RingRowLayout.PrivateBytesOffset, (long)Marshal.OffsetOf<RingRowBlittable>(nameof(RingRowBlittable.PrivateBytes)));
        Assert.Equal(RingRowLayout.ReadBpsOffset, (long)Marshal.OffsetOf<RingRowBlittable>(nameof(RingRowBlittable.ReadBps)));
        Assert.Equal(RingRowLayout.WriteBpsOffset, (long)Marshal.OffsetOf<RingRowBlittable>(nameof(RingRowBlittable.WriteBps)));
        Assert.Equal(RingRowLayout.GpuDedicatedBytesOffset, (long)Marshal.OffsetOf<RingRowBlittable>(nameof(RingRowBlittable.GpuDedicatedBytes)));
        Assert.Equal(RingRowLayout.GpuSharedBytesOffset, (long)Marshal.OffsetOf<RingRowBlittable>(nameof(RingRowBlittable.GpuSharedBytes)));
        Assert.Equal(RingRowLayout.NameOffset, (long)Marshal.OffsetOf<RingRowBlittable>(nameof(RingRowBlittable.Name)));
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
