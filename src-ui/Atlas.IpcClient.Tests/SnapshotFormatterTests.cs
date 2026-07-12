using Atlas.IpcClient;
using Atlas.V0;
using Xunit;

namespace Atlas.IpcClient.Tests;

public class SnapshotFormatterTests
{
    [Fact]
    public void Percent_ConvertsPermille()
    {
        Assert.Equal(0.0, SnapshotFormatter.Percent(0));
        Assert.Equal(12.3, SnapshotFormatter.Percent(123), 3);
        Assert.Equal(100.0, SnapshotFormatter.Percent(1000), 3);
    }

    [Fact]
    public void Mb_And_Gb_Convert()
    {
        Assert.Equal(1.0, SnapshotFormatter.Mb(1024UL * 1024UL), 6);
        Assert.Equal(1.0, SnapshotFormatter.Gb(1024UL * 1024UL * 1024UL), 6);
    }

    [Fact]
    public void Truncate_CapsAtMaxLength()
    {
        Assert.Equal("abc", SnapshotFormatter.Truncate("abc", 30));
        Assert.Equal("abcde", SnapshotFormatter.Truncate("abcdefghij", 5));
    }

    [Fact]
    public void HeaderRow_HasExpectedColumns()
    {
        var header = SnapshotFormatter.HeaderRow();
        Assert.Contains("PID", header);
        Assert.Contains("NAME", header);
        Assert.Contains("CPU%", header);
        Assert.Contains("WS MB", header);
        Assert.Contains("PRIV MB", header);
        Assert.Contains("THR", header);
        Assert.Contains("HANDLE", header);
    }

    [Fact]
    public void ProcessRowLine_FormatsFields()
    {
        var row = new ProcessRow
        {
            Pid = 4242,
            ImageName = "explorer.exe",
            CpuPermille = 125,                 // 12.5%
            WorkingSet = 200UL * 1024 * 1024,  // 200.0 MB
            PrivateBytes = 100UL * 1024 * 1024, // 100.0 MB
            ThreadCount = 33,
            HandleCount = 777,
        };

        var line = SnapshotFormatter.ProcessRowLine(row);
        Assert.Contains("4242", line);
        Assert.Contains("explorer.exe", line);
        Assert.Contains("12.5", line);
        Assert.Contains("200.0", line);
        Assert.Contains("100.0", line);
        Assert.Contains("33", line);
        Assert.Contains("777", line);
    }

    [Fact]
    public void SystemLine_EmptyWhenNoGauges()
    {
        var reply = new SnapshotReply(); // System is null
        Assert.Equal(string.Empty, SnapshotFormatter.SystemLine(reply));
    }

    [Fact]
    public void SystemLine_RendersGauges()
    {
        var reply = new SnapshotReply
        {
            System = new SystemGauges
            {
                CpuPermille = 250,                    // 25.0%
                MemUsed = 8UL * 1024 * 1024 * 1024,   // 8.0 GB
                MemTotal = 16UL * 1024 * 1024 * 1024, // 16.0 GB
                ProcessCount = 300,
                ThreadCount = 4000,
                HandleCount = 90000,
            },
        };

        var line = SnapshotFormatter.SystemLine(reply);
        Assert.Contains("25.0%", line);
        Assert.Contains("8.0/16.0 GB", line);
        Assert.Contains("300 processes", line);
    }

    [Fact]
    public void WatchLine_SummarizesTopProcess()
    {
        var reply = new SnapshotReply
        {
            System = new SystemGauges { CpuPermille = 421 },
        };
        reply.Processes.Add(new ProcessRow { ImageName = "hog.exe", CpuPermille = 380 });
        reply.Processes.Add(new ProcessRow { ImageName = "idle.exe", CpuPermille = 0 });

        var line = SnapshotFormatter.WatchLine(reply);
        Assert.Contains("42.1%", line);
        Assert.Contains("procs", line);
        Assert.Contains("hog.exe 38.0%", line);
    }

    [Fact]
    public void WatchLine_HandlesEmptyProcessList()
    {
        var reply = new SnapshotReply { System = new SystemGauges { CpuPermille = 0 } };
        var line = SnapshotFormatter.WatchLine(reply);
        Assert.Contains("top: -", line);
    }
}
