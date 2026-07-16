using Atlas.V0;
using Xunit;

namespace Atlas.IpcClient.Tests;

public sealed class InsightFormatterTests
{
    [Theory]
    [InlineData(InsightStatus.Active, "Needs attention")]
    [InlineData(InsightStatus.Emerging, "Watch")]
    [InlineData(InsightStatus.Clear, "Clear")]
    [InlineData(InsightStatus.Limited, "Limited data")]
    public void Status_labels_are_plain_language(InsightStatus status, string expected)
    {
        Assert.Equal(expected, InsightFormatter.StatusLabel(status));
    }

    [Theory]
    [InlineData("process:42:100:example.exe", "Inspect process")]
    [InlineData("activity", "Open Live Activity")]
    [InlineData("graphics", "Open Graphics")]
    [InlineData("", "")]
    public void Action_labels_follow_destinations(string destination, string expected)
    {
        Assert.Equal(expected, InsightFormatter.ActionLabel(destination));
    }

    [Fact]
    public void Evidence_summary_is_explicit_when_measurements_are_missing()
    {
        Assert.Equal(
            "No measurement is available yet.",
            InsightFormatter.EvidenceSummary(new Insight()));
    }

    [Fact]
    public void Evidence_summary_names_count_and_first_measurement()
    {
        var insight = new Insight();
        insight.Evidence.Add(new EvidenceItem { Text = "CPU is 92%." });
        insight.Evidence.Add(new EvidenceItem { Text = "The incident lasted 3 minutes." });

        Assert.Equal(
            "Based on 2 measurements. CPU is 92%.",
            InsightFormatter.EvidenceSummary(insight));
    }

    [Fact]
    public void Process_destination_round_trips_identity()
    {
        var parsed = InsightFormatter.TryParseProcessDestination(
            "process:4242:987654:example.exe",
            out var pid,
            out var createTime,
            out var imageName);

        Assert.True(parsed);
        Assert.Equal((uint)4242, pid);
        Assert.Equal(987654, createTime);
        Assert.Equal("example.exe", imageName);
    }

    [Theory]
    [InlineData("activity")]
    [InlineData("process:not-a-pid:10:test.exe")]
    [InlineData("process:0:10:test.exe")]
    public void Invalid_process_destinations_do_not_open_an_inspector(string destination)
    {
        Assert.False(InsightFormatter.TryParseProcessDestination(
            destination, out _, out _, out _));
    }
}
