using System.Threading;
using Atlas.IpcClient;
using Xunit;

namespace Atlas.IpcClient.Tests;

/// <summary>
/// Behavioral tests for the ring reader, exercised against an in-process C#
/// writer (<see cref="TestRingWriter"/>) that reproduces the Rust layout and
/// seqlock. Each test uses a unique discriminator so parallel runs never collide
/// on the shared section namespace.
/// </summary>
public class MetricsRingTests
{
    private static string Disc(string tag) =>
        $"uitest.{tag}.{Environment.ProcessId}.{Guid.NewGuid():N}";

    [Fact]
    public void RoundTrip_ReadsGaugesAndRows()
    {
        var who = Disc("round_trip");
        using var writer = TestRingWriter.Create(who);
        writer.Publish(new TestUpdate
        {
            TsMs = 42,
            CpuPermille = 333,
            Rows =
            {
                new TestRow { Pid = 4, CpuPermille = 250, WorkingSet = 1 << 20, PrivateBytes = 2 << 20, ReadBps = 1000, WriteBps = 2000, Name = "system.exe" },
                new TestRow { Pid = 1234, CpuPermille = 125, WorkingSet = 3 << 20, PrivateBytes = 4 << 20, Name = "notepad.exe" },
            },
        });

        var result = MetricsRing.TryOpen(who);
        Assert.Equal(RingOpenStatus.Opened, result.Status);
        using var ring = result.Ring!;

        var snap = ring.Snapshot();
        Assert.NotNull(snap);
        Assert.Equal(42, snap!.TsMs);
        Assert.Equal(333u, snap.CpuPermille);
        Assert.Equal(100u, snap.ProcessCount);
        Assert.Equal(2000u, snap.ThreadCount);
        Assert.Equal(40000u, snap.HandleCount);
        Assert.Equal(8UL << 30, snap.MemUsed);
        Assert.Equal(16UL << 30, snap.MemTotal);

        Assert.Equal(2, snap.Rows.Count);
        Assert.Equal(4u, snap.Rows[0].Pid);
        Assert.Equal("system.exe", snap.Rows[0].Name);
        Assert.Equal(250u, snap.Rows[0].CpuPermille);
        Assert.Equal(1UL << 20, snap.Rows[0].WorkingSet);
        Assert.Equal(2000UL, snap.Rows[0].WriteBps);
        Assert.Equal(1234u, snap.Rows[1].Pid);
        Assert.Equal("notepad.exe", snap.Rows[1].Name);
    }

    [Fact]
    public void EmptyRing_BeforeFirstPublish_ReadsZeroRows()
    {
        // A reader attaching after create but before any publish gets a valid,
        // empty snapshot (magic/version stamped, seq even at 0).
        var who = Disc("empty");
        using var writer = TestRingWriter.Create(who);

        var result = MetricsRing.TryOpen(who);
        Assert.Equal(RingOpenStatus.Opened, result.Status);
        using var ring = result.Ring!;

        var snap = ring.Snapshot();
        Assert.NotNull(snap);
        Assert.Empty(snap!.Rows);
        Assert.Equal(0, snap.TsMs);
    }

    [Fact]
    public void LongName_TruncatesAtRingNameLen()
    {
        var who = Disc("truncate");
        using var writer = TestRingWriter.Create(who);
        var longName = "this-is-a-very-long-process-name-that-exceeds-the-limit.exe";
        Assert.True(longName.Length > MetricsRing.RingNameLen);
        writer.Publish(new TestUpdate
        {
            TsMs = 1,
            Rows = { new TestRow { Pid = 1, Name = longName } },
        });

        using var ring = MetricsRing.TryOpen(who).Ring!;
        var snap = ring.Snapshot()!;
        var got = snap.Rows[0].Name;
        Assert.Equal(MetricsRing.RingNameLen, got.Length);
        Assert.Equal(longName.Substring(0, MetricsRing.RingNameLen), got);
    }

    [Fact]
    public void ShrinkingRowCount_LeavesNoStaleRows()
    {
        var who = Disc("shrink");
        using var writer = TestRingWriter.Create(who);
        var many = new TestUpdate { TsMs = 1 };
        for (uint i = 0; i < 5; i++)
        {
            many.Rows.Add(new TestRow { Pid = i + 1, Name = "p.exe" });
        }
        writer.Publish(many);
        writer.Publish(new TestUpdate { TsMs = 2, Rows = { new TestRow { Pid = 99, Name = "one.exe" } } });

        using var ring = MetricsRing.TryOpen(who).Ring!;
        var snap = ring.Snapshot()!;
        Assert.Single(snap.Rows);
        Assert.Equal(99u, snap.Rows[0].Pid);
    }

    [Fact]
    public void MissingSection_ReturnsNotFound()
    {
        var who = Disc("missing");
        var result = MetricsRing.TryOpen(who);
        Assert.Equal(RingOpenStatus.NotFound, result.Status);
        Assert.Null(result.Ring);
    }

    [Fact]
    public void MagicMismatch_ReturnsIncompatible()
    {
        var who = Disc("badmagic");
        using var writer = TestRingWriter.Create(who, magic: 0xDEADBEEF);
        var result = MetricsRing.TryOpen(who);
        Assert.Equal(RingOpenStatus.Incompatible, result.Status);
        Assert.Null(result.Ring);
        Assert.Contains("magic", result.Message);
    }

    [Fact]
    public void VersionMismatch_ReturnsIncompatible()
    {
        var who = Disc("badversion");
        using var writer = TestRingWriter.Create(who, version: MetricsRing.LayoutVersion + 1);
        var result = MetricsRing.TryOpen(who);
        Assert.Equal(RingOpenStatus.Incompatible, result.Status);
        Assert.Null(result.Ring);
        Assert.Contains("version", result.Message);
    }

    /// <summary>
    /// Concurrency invariant: while a writer republishes in a tight loop, every
    /// reader snapshot must be internally consistent. Each field derives from a
    /// single per-publish counter <c>c</c>, so any torn read (fields from two
    /// publishes mixed) is detectable as a broken invariant — mirroring the Rust
    /// <c>reader_sees_consistent_data_under_concurrent_writes</c> test.
    /// </summary>
    [Fact]
    public void TornWrite_RetryYieldsConsistentSnapshots()
    {
        var who = Disc("concurrent");
        using var writer = TestRingWriter.Create(who);
        using var stop = new ManualResetEventSlim(false);

        var writerThread = new Thread(() =>
        {
            uint c = 1;
            while (!stop.IsSet)
            {
                var u = new TestUpdate { TsMs = c, CpuPermille = c };
                for (uint i = 0; i < 8; i++)
                {
                    u.Rows.Add(new TestRow
                    {
                        Pid = c + i,
                        CpuPermille = c,
                        WorkingSet = (ulong)c * 1000 + i,
                        PrivateBytes = (ulong)c * 7,
                        ReadBps = c,
                        WriteBps = c,
                        Name = "x.exe",
                    });
                }
                writer.Publish(u);
                c = c == uint.MaxValue ? 1 : c + 1;
            }
        })
        { IsBackground = true };
        writerThread.Start();

        using var ring = MetricsRing.TryOpen(who).Ring!;
        int consistent = 0;
        int retried = 0;
        for (int iter = 0; iter < 20_000; iter++)
        {
            var snap = ring.Snapshot();
            if (snap is null)
            {
                retried++;
                continue;
            }
            // Every field must agree with the header's cpu-derived counter.
            uint c = snap.CpuPermille;
            Assert.Equal((long)c, snap.TsMs);
            for (int i = 0; i < snap.Rows.Count; i++)
            {
                var row = snap.Rows[i];
                Assert.Equal(c, row.CpuPermille);
                Assert.Equal(c + (uint)i, row.Pid);
                Assert.Equal((ulong)c * 1000 + (ulong)i, row.WorkingSet);
                Assert.Equal((ulong)c * 7, row.PrivateBytes);
            }
            consistent++;
        }

        stop.Set();
        writerThread.Join();

        // Liveness: a large number of consistent snapshots even under a hot
        // writer. The per-field asserts above are the real safety check; a null
        // snapshot (publish raced the copy) is a correct outcome, so `retried`
        // being non-zero is expected and fine.
        Assert.True(consistent > 1000, $"reader starved: {consistent} consistent, {retried} retried");
    }
}
