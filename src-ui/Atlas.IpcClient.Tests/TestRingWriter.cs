using System.IO.MemoryMappedFiles;
using System.Runtime.CompilerServices;
using System.Threading;
using Atlas.IpcClient;
using Microsoft.Win32.SafeHandles;

namespace Atlas.IpcClient.Tests;

/// <summary>
/// An in-process test writer that reproduces the Rust <c>RingWriter</c> layout
/// and seqlock discipline in C#, so the reader can be exercised without a live
/// service. Creates the named section with <c>CreateNew</c>, stamps the header,
/// and publishes updates by writing raw fields at the pinned offsets under the
/// same odd/even seq protocol as shm.rs.
/// </summary>
[System.Runtime.Versioning.SupportedOSPlatform("windows")]
internal sealed unsafe class TestRingWriter : IDisposable
{
    private readonly MemoryMappedFile _mmf;
    private readonly MemoryMappedViewAccessor _view;
    private readonly SafeMemoryMappedViewHandle _handle;
    private readonly byte* _base;
    private bool _disposed;

    /// <summary>The section name this writer created (for the reader to open).</summary>
    public string SectionName { get; }

    private TestRingWriter(string sectionName, MemoryMappedFile mmf, MemoryMappedViewAccessor view)
    {
        SectionName = sectionName;
        _mmf = mmf;
        _view = view;
        _handle = view.SafeMemoryMappedViewHandle;
        byte* ptr = null;
        _handle.AcquirePointer(ref ptr);
        _base = ptr + view.PointerOffset;
    }

    /// <summary>
    /// Creates a fresh section for <paramref name="who"/> and stamps a clean,
    /// stable (even seq = 0) empty header with valid magic + version. Optionally
    /// overrides magic/version to construct an "incompatible" section for the
    /// rejection test.
    /// </summary>
    public static TestRingWriter Create(string who, uint magic = MetricsRing.RingMagic, uint version = MetricsRing.LayoutVersion)
    {
        var name = MetricsRing.SectionName(who);
        var mmf = MemoryMappedFile.CreateNew(name, RingLayout.Size, MemoryMappedFileAccess.ReadWrite);
        var view = mmf.CreateViewAccessor(0, RingLayout.Size, MemoryMappedFileAccess.ReadWrite);
        var w = new TestRingWriter(name, mmf, view);
        w.InitHeader(magic, version);
        return w;
    }

    private void InitHeader(uint magic, uint version)
    {
        WriteU32(RingHeaderLayout.MagicOffset, magic);
        WriteU32(RingHeaderLayout.LayoutVersionOffset, version);
        WriteU32(RingHeaderLayout.SeqOffset, 0);
        WriteU32(RingHeaderLayout.PadOffset, 0);
        WriteI64(RingHeaderLayout.TsMsOffset, 0);
        WriteU32(RingHeaderLayout.CpuPermilleOffset, 0);
        WriteU32(RingHeaderLayout.ProcessCountOffset, 0);
        WriteU32(RingHeaderLayout.ThreadCountOffset, 0);
        WriteU32(RingHeaderLayout.HandleCountOffset, 0);
        WriteU64(RingHeaderLayout.MemUsedOffset, 0);
        WriteU64(RingHeaderLayout.MemTotalOffset, 0);
        WriteU64(RingHeaderLayout.CommitUsedOffset, 0);
        WriteU64(RingHeaderLayout.CommitLimitOffset, 0);
        WriteU32(RingHeaderLayout.RowCountOffset, 0);
        WriteU32(RingHeaderLayout.Pad2Offset, 0);
        for (int i = 0; i < MetricsRing.RingRows; i++)
        {
            ZeroRow(i);
        }
    }

    /// <summary>Publishes one update under the seqlock (odd, write, even).</summary>
    public void Publish(TestUpdate u)
    {
        uint start = ReadU32(RingHeaderLayout.SeqOffset);
        Volatile.Write(ref SeqRef(), (int)(start + 1)); // odd
        Interlocked.MemoryBarrier();

        WriteI64(RingHeaderLayout.TsMsOffset, u.TsMs);
        WriteU32(RingHeaderLayout.CpuPermilleOffset, u.CpuPermille);
        WriteU32(RingHeaderLayout.ProcessCountOffset, u.ProcessCount);
        WriteU32(RingHeaderLayout.ThreadCountOffset, u.ThreadCount);
        WriteU32(RingHeaderLayout.HandleCountOffset, u.HandleCount);
        WriteU64(RingHeaderLayout.MemUsedOffset, u.MemUsed);
        WriteU64(RingHeaderLayout.MemTotalOffset, u.MemTotal);
        WriteU64(RingHeaderLayout.CommitUsedOffset, u.CommitUsed);
        WriteU64(RingHeaderLayout.CommitLimitOffset, u.CommitLimit);

        int n = Math.Min(u.Rows.Count, MetricsRing.RingRows);
        WriteU32(RingHeaderLayout.RowCountOffset, (uint)n);
        for (int i = 0; i < n; i++)
        {
            WriteRow(i, u.Rows[i]);
        }
        for (int i = n; i < MetricsRing.RingRows; i++)
        {
            ZeroRow(i);
        }

        Interlocked.MemoryBarrier();
        Volatile.Write(ref SeqRef(), (int)(start + 2)); // even
    }

    private void WriteRow(int i, TestRow r)
    {
        long b = RingLayout.RowsOffset + (long)i * RingRowLayout.Size;
        WriteU32(b + RingRowLayout.PidOffset, r.Pid);
        WriteU32(b + RingRowLayout.CpuPermilleOffset, r.CpuPermille);
        WriteU64(b + RingRowLayout.WorkingSetOffset, r.WorkingSet);
        WriteU64(b + RingRowLayout.PrivateBytesOffset, r.PrivateBytes);
        WriteU64(b + RingRowLayout.ReadBpsOffset, r.ReadBps);
        WriteU64(b + RingRowLayout.WriteBpsOffset, r.WriteBps);
        EncodeName(b + RingRowLayout.NameOffset, r.Name);
    }

    private void ZeroRow(int i)
    {
        long b = RingLayout.RowsOffset + (long)i * RingRowLayout.Size;
        for (long off = 0; off < RingRowLayout.Size; off += sizeof(ulong))
        {
            WriteU64(b + off, 0);
        }
    }

    /// <summary>
    /// Encodes a name as NUL-padded, truncated UTF-16 (mirrors the Rust
    /// <c>encode_name</c>: cut at RING_NAME_LEN code units, zero the tail).
    /// </summary>
    private void EncodeName(long offset, string name)
    {
        for (int u = 0; u < MetricsRing.RingNameLen; u++)
        {
            ushort unit = u < name.Length ? name[u] : (ushort)0;
            WriteU16(offset + (long)u * sizeof(ushort), unit);
        }
    }

    private ref int SeqRef() =>
        ref Unsafe.AsRef<int>(_base + RingHeaderLayout.SeqOffset);

    private uint ReadU32(long off) => Unsafe.ReadUnaligned<uint>(_base + off);
    private void WriteU32(long off, uint v) => Unsafe.WriteUnaligned(_base + off, v);
    private void WriteU64(long off, ulong v) => Unsafe.WriteUnaligned(_base + off, v);
    private void WriteI64(long off, long v) => Unsafe.WriteUnaligned(_base + off, v);
    private void WriteU16(long off, ushort v) => Unsafe.WriteUnaligned(_base + off, v);

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

internal sealed class TestUpdate
{
    public long TsMs;
    public uint CpuPermille;
    public uint ProcessCount = 100;
    public uint ThreadCount = 2000;
    public uint HandleCount = 40000;
    public ulong MemUsed = 8UL << 30;
    public ulong MemTotal = 16UL << 30;
    public ulong CommitUsed = 9UL << 30;
    public ulong CommitLimit = 20UL << 30;
    public List<TestRow> Rows = new();
}

internal sealed class TestRow
{
    public uint Pid;
    public uint CpuPermille;
    public ulong WorkingSet;
    public ulong PrivateBytes;
    public ulong ReadBps;
    public ulong WriteBps;
    public string Name = string.Empty;
}
