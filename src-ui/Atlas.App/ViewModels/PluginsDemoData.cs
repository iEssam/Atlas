using System;
using System.Collections.Generic;
using Atlas.V0;

namespace Atlas.App.ViewModels;

/// <summary>
/// Sample plugin-registry data so the Plugins page can be previewed without the
/// AtlasPlugins backend (which lands after this UI — task brief). Gated behind
/// <c>ATLAS_FAKE_PLUGINS=1</c>; never used against a live service. The data is
/// representative — a signed plugin that's on with a couple of read-only grants, a
/// signed plugin that's off, and an unsigned one — so the whole UX, including the
/// calm signature framing and per-capability grants, can be seen end to end.
/// </summary>
internal static class PluginsDemoData
{
    public static IReadOnlyList<Plugin> SamplePlugins()
    {
        long now = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
        return new List<Plugin>
        {
            new Plugin
            {
                Id = 1,
                Name = "Timeline Insights",
                Version = "1.2.0",
                Publisher = "Contoso Ltd.",
                ExePath = @"C:\Program Files\Atlas Plugins\timeline-insights\timeline-insights.exe",
                Signature = PluginSignature.PluginSigned,
                Enabled = true,
                RegisteredMs = now - 6L * 24 * 60 * 60_000,
                Description = "Adds richer timeline analytics over your history.",
                Granted =
                {
                    PluginCapability.PluginCapSnapshot,
                    PluginCapability.PluginCapHistory,
                    PluginCapability.PluginCapIncidents,
                },
            },
            new Plugin
            {
                Id = 2,
                Name = "Net Watch",
                Version = "0.9.1",
                Publisher = "Fabrikam, Inc.",
                ExePath = @"C:\Program Files\Atlas Plugins\net-watch\net-watch.exe",
                Signature = PluginSignature.PluginSigned,
                Enabled = false,
                RegisteredMs = now - 2L * 24 * 60 * 60_000,
                Description = "Surfaces connection and listening-port summaries.",
                Granted =
                {
                    PluginCapability.PluginCapNetwork,
                },
            },
            new Plugin
            {
                Id = 3,
                Name = "Scratch Exporter",
                Version = "0.1.0",
                Publisher = string.Empty,
                ExePath = @"C:\Users\dev\tools\scratch-exporter-unsigned.exe",
                Signature = PluginSignature.PluginUnsigned,
                Enabled = false,
                RegisteredMs = now - 3 * 60 * 60_000,
                Description = "A local, unsigned build — enable only if you trust the source.",
                Granted =
                {
                    PluginCapability.PluginCapInventory,
                },
            },
        };
    }
}
