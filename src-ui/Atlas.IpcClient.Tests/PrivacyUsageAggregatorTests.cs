using Atlas.V0;
using Xunit;

namespace Atlas.IpcClient.Tests;

public sealed class PrivacyUsageAggregatorTests
{
    [Fact]
    public void DuplicateDesktopRecordsBecomeOneAppSummary()
    {
        var summaries = PrivacyUsageAggregator.Aggregate(
        [
            new PrivacyUsage
            {
                Capability = CapabilityKind.Microphone,
                AppId = "discord-device-1",
                DisplayName = "Discord.exe",
                LastStartMs = 100,
                LastStopMs = 120,
            },
            new PrivacyUsage
            {
                Capability = CapabilityKind.Microphone,
                AppId = "discord-device-2",
                DisplayName = "discord.EXE",
                InUse = true,
                LastStartMs = 200,
            },
        ]);

        var summary = Assert.Single(summaries);
        Assert.Equal(CapabilityKind.Microphone, summary.Capability);
        Assert.Equal("Discord.exe", summary.DisplayName);
        Assert.True(summary.InUse);
        Assert.Equal(200, summary.LastStartMs);
        Assert.Equal(120, summary.LastStopMs);
        Assert.Equal(2, summary.RecordCount);
    }

    [Fact]
    public void CapabilityAndAppTypeRemainSeparate()
    {
        var summaries = PrivacyUsageAggregator.Aggregate(
        [
            new PrivacyUsage { Capability = CapabilityKind.Camera, DisplayName = "Teams", Packaged = true },
            new PrivacyUsage { Capability = CapabilityKind.Microphone, DisplayName = "Teams", Packaged = true },
            new PrivacyUsage { Capability = CapabilityKind.Camera, DisplayName = "Teams", Packaged = false },
        ]);

        Assert.Equal(3, summaries.Count);
    }
}
