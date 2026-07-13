using Atlas.IpcClient;
using Atlas.V0;
using Xunit;

namespace Atlas.IpcClient.Tests;

public class HistoryFormatterTests
{
    [Fact]
    public void ToCpuTimeline_ScalesPermilleToPercent_AndSortsByTime()
    {
        var buckets = new[]
        {
            new RangeBucket { StartMs = 2000, Min = 100, Max = 300, Avg = 200, Samples = 5 },
            new RangeBucket { StartMs = 1000, Min = 0, Max = 1000, Avg = 500, Samples = 3 },
        };

        var points = HistoryFormatter.ToCpuTimeline(buckets);

        Assert.Equal(2, points.Count);
        // Sorted ascending by StartMs.
        Assert.Equal(1000, points[0].StartMs);
        Assert.Equal(2000, points[1].StartMs);
        // Permille → percent.
        Assert.Equal(0.0, points[0].MinPercent, 3);
        Assert.Equal(100.0, points[0].MaxPercent, 3);
        Assert.Equal(50.0, points[0].AvgPercent, 3);
        Assert.Equal(20.0, points[1].AvgPercent, 3);
        Assert.Equal(5u, points[1].Samples);
    }

    [Fact]
    public void ToCpuTimeline_EmptyInput_YieldsEmpty()
    {
        Assert.Empty(HistoryFormatter.ToCpuTimeline(System.Array.Empty<RangeBucket>()));
    }

    [Theory]
    [InlineData(1000, 2000, 1000, false)] // exactly one step apart — contiguous
    [InlineData(1000, 2400, 1000, false)] // within 50% tolerance
    [InlineData(1000, 4000, 1000, true)]  // 3 steps — a gap
    [InlineData(1000, 2000, 0, false)]    // unknown step — never a gap
    public void IsGap_DetectsBreaksBeyondTolerance(long a, long b, long step, bool expected)
    {
        Assert.Equal(expected, HistoryFormatter.IsGap(a, b, step));
    }

    [Fact]
    public void EventLine_Start_NoExitCode()
    {
        var e = new EventRow { Kind = 0, Pid = 4242, ImageName = "chrome.exe" };
        Assert.Equal("chrome.exe (pid 4242) started", HistoryFormatter.EventLine(e));
    }

    [Fact]
    public void EventLine_Stop_WithExitCode()
    {
        var e = new EventRow
        {
            Kind = 1, Pid = 900, ImageName = "notepad.exe",
            ExitStatus = 1, HasExitStatus = true,
        };
        Assert.Equal("notepad.exe (pid 900) exited (code 1)", HistoryFormatter.EventLine(e));
    }

    [Fact]
    public void EventLine_UnknownImage_FallsBack()
    {
        var e = new EventRow { Kind = 0, Pid = 7 };
        Assert.Contains("(unknown)", HistoryFormatter.EventLine(e));
    }

    [Theory]
    [InlineData(ProcessActionKind.CloseWindows, "Close")]
    [InlineData(ProcessActionKind.Suspend, "Suspend")]
    [InlineData(ProcessActionKind.Resume, "Resume")]
    [InlineData(ProcessActionKind.Terminate, "End")]
    public void ActionVerb_MapsKinds(ProcessActionKind kind, string expected)
    {
        Assert.Equal(expected, HistoryFormatter.ActionVerb(kind));
    }

    [Theory]
    [InlineData(ProcessActionKind.Suspend, true)]
    [InlineData(ProcessActionKind.Resume, true)]
    [InlineData(ProcessActionKind.CloseWindows, true)]
    [InlineData(ProcessActionKind.Terminate, false)]
    public void IsReversible_TerminateIsNot(ProcessActionKind kind, bool reversible)
    {
        Assert.Equal(reversible, HistoryFormatter.IsReversible(kind));
        Assert.Contains(
            reversible ? "reversible" : "not reversible",
            HistoryFormatter.ReversibilityText(kind));
    }

    [Fact]
    public void RiskSummary_Null_IsEmpty()
    {
        Assert.Equal(string.Empty, HistoryFormatter.RiskSummary(null));
    }

    [Fact]
    public void RiskSummary_RendersFlagsCountsAndNotes()
    {
        var risk = new ActionRisk
        {
            IsCritical = true,
            IsSystem = true,
            VisibleWindows = 2,
            ChildCount = 1,
        };
        risk.Notes.Add("children will become orphans");

        var text = HistoryFormatter.RiskSummary(risk);

        Assert.Contains("critical", text);
        Assert.Contains("SYSTEM", text);
        Assert.Contains("2 visible windows", text);
        Assert.Contains("1 child process", text);
        Assert.Contains("children will become orphans", text);
    }

    [Fact]
    public void RiskSummary_SingularPluralization()
    {
        var risk = new ActionRisk { VisibleWindows = 1, ChildCount = 2 };
        var text = HistoryFormatter.RiskSummary(risk);
        Assert.Contains("1 visible window ", text + " "); // singular, no trailing 's'
        Assert.DoesNotContain("1 visible windows", text);
        Assert.Contains("2 child processes", text);
    }

    [Fact]
    public void RiskSummary_SkipsBlankNotes()
    {
        var risk = new ActionRisk();
        risk.Notes.Add("   ");
        Assert.Equal(string.Empty, HistoryFormatter.RiskSummary(risk));
    }
}
