using Atlas.IpcClient;

// Atlas DevCli — the Stage 1 interop proof. Connects to a running
// `atlas-service serve --pipe <who>` over the Windows named pipe, prints
// capabilities and a top-N process table, and (with --watch) streams updates.
//
// Usage:
//   atlas-devcli [--pipe <who>] [--top <n>] [--watch]
//
//   --pipe <who>   discriminator matching the server's --pipe flag (default:
//                  USERNAME, same as the Rust default).
//   --top <n>      number of rows (0 = all). Default 15.
//   --watch        stream ~1 Hz updates until Ctrl+C.

string? who = null;
uint topN = 15;
bool watch = false;
bool probeR2 = false;
bool probeRules = false;
bool probePrivacyAlerts = false;
bool probeSupportBundle = false;
bool probePlugins = false;
uint probePid = 0;

for (int i = 0; i < args.Length; i++)
{
    switch (args[i])
    {
        case "--pipe" when i + 1 < args.Length:
            who = args[++i];
            break;
        case "--top" when i + 1 < args.Length:
            if (!uint.TryParse(args[++i], out topN))
            {
                Console.Error.WriteLine($"invalid --top value: {args[i]}");
                return 2;
            }
            break;
        case "--watch":
            watch = true;
            break;
        // R2 live check: exercise the deep-inspector / resource-owner RPCs and
        // report how each degrades (Supported vs Unsupported→"server too old").
        case "--probe-r2":
            probeR2 = true;
            break;
        // R2 rules-engine live check: exercise the AtlasRules RPCs and report how
        // each degrades (Supported vs Unsupported→"server too old").
        case "--probe-rules":
            probeRules = true;
            break;
        // R2 privacy-alerts live check: exercise the 5 privacy-alert RPCs and
        // report how each degrades (Supported vs Unsupported→"server too old").
        case "--probe-privacy-alerts":
            probePrivacyAlerts = true;
            break;
        // R3 support-bundle live check: exercise GenerateSupportBundle and report
        // how it degrades (Supported vs Unsupported→"server too old").
        case "--probe-support-bundle":
            probeSupportBundle = true;
            break;
        // R3 plugins live check: exercise the AtlasPlugins registry RPCs and report
        // how each degrades (Supported vs Unsupported→"server too old").
        case "--probe-plugins":
            probePlugins = true;
            break;
        case "--pid" when i + 1 < args.Length:
            if (!uint.TryParse(args[++i], out probePid))
            {
                Console.Error.WriteLine($"invalid --pid value: {args[i]}");
                return 2;
            }
            break;
        case "-h":
        case "--help":
            Console.WriteLine(
                "usage: atlas-devcli [--pipe <who>] [--top <n>] [--watch] "
                + "[--probe-r2 [--pid <pid>]] [--probe-rules] [--probe-privacy-alerts] "
                + "[--probe-support-bundle] [--probe-plugins]");
            return 0;
        default:
            Console.Error.WriteLine($"unknown argument: {args[i]}");
            return 2;
    }
}

if (probeR2)
{
    return await ProbeR2Async(who, probePid);
}

if (probeRules)
{
    return await ProbeRulesAsync(who);
}

if (probePrivacyAlerts)
{
    return await ProbePrivacyAlertsAsync(who);
}

if (probeSupportBundle)
{
    return await ProbeSupportBundleAsync(who);
}

if (probePlugins)
{
    return await ProbePluginsAsync(who);
}

var pipePath = AtlasPipe.FullPath(who ?? AtlasPipe.DefaultWho());
Console.WriteLine($"Connecting to {pipePath} ...");

using var cts = new CancellationTokenSource();
Console.CancelKeyPress += (_, e) =>
{
    e.Cancel = true;      // let us shut the stream down gracefully
    cts.Cancel();
};

try
{
    using var atlas = AtlasChannel.Connect(who);

    var caps = await atlas.GetCapabilitiesAsync(cts.Token);
    Console.WriteLine(
        $"Capabilities: service_version={caps.ServiceVersion} " +
        $"flags=[{string.Join(", ", caps.CapabilityFlags)}]");
    Console.WriteLine();

    if (watch)
    {
        Console.WriteLine("Streaming snapshots (Ctrl+C to stop)");
        try
        {
            await foreach (var reply in atlas.StreamSnapshotsAsync(topN, cts.Token))
            {
                Console.WriteLine(SnapshotFormatter.WatchLine(reply));
            }
        }
        catch (OperationCanceledException)
        {
            // Ctrl+C — normal exit.
        }
    }
    else
    {
        var reply = await atlas.GetSnapshotAsync(topN, cts.Token);
        Console.Write(SnapshotFormatter.RenderTable(reply));
    }

    return 0;
}
catch (OperationCanceledException)
{
    return 0;
}
catch (Exception ex)
{
    Console.Error.WriteLine($"error: {ex.Message}");
    return 1;
}

// Exercises the R2 RPCs (GetProcessDetail / ListHandles / ListModules /
// ListThreads / FindResourceOwners) and prints, for each, whether the server
// supported it or degraded to Unsupported. This is the console equivalent of
// navigating the UI's Inspector / File-Lock pages: against a server too old to
// serve these RPCs, every call should come back Unsupported (not throw).
static async Task<int> ProbeR2Async(string? who, uint pid)
{
    try
    {
        using var atlas = AtlasChannel.Connect(who);

        if (pid == 0)
        {
            var snap = await atlas.GetSnapshotAsync(1);
            if (snap.Processes.Count > 0)
            {
                pid = snap.Processes[0].Pid;
            }
        }
        Console.WriteLine($"Probing R2 RPCs against pid {pid} ...");
        Console.WriteLine();

        var detail = await atlas.GetProcessDetailAsync(pid, 0);
        Console.WriteLine(detail.Supported
            ? $"GetProcessDetail   : Supported (available={detail.Value.Available}, "
              + $"limited={(detail.Value.Available && detail.Value.Detail.Limited)})"
            : $"GetProcessDetail   : Unsupported — {detail.UnsupportedReason}");

        var handles = await atlas.ListHandlesAsync(pid);
        Console.WriteLine(handles.Supported
            ? $"ListHandles        : Supported ({handles.Value.Handles.Count} handles, "
              + $"namesLimited={handles.Value.NamesLimited})"
            : $"ListHandles        : Unsupported — {handles.UnsupportedReason}");

        var modules = await atlas.ListModulesAsync(pid);
        Console.WriteLine(modules.Supported
            ? $"ListModules        : Supported (available={modules.Value.Available}, "
              + $"{modules.Value.Modules.Count} modules)"
            : $"ListModules        : Unsupported — {modules.UnsupportedReason}");

        var threads = await atlas.ListThreadsAsync(pid);
        Console.WriteLine(threads.Supported
            ? $"ListThreads        : Supported ({threads.Value.Threads.Count} threads)"
            : $"ListThreads        : Unsupported — {threads.UnsupportedReason}");

        var owners = await atlas.FindResourceOwnersAsync(@"C:\Windows\System32\notepad.exe");
        Console.WriteLine(owners.Supported
            ? $"FindResourceOwners : Supported (available={owners.Value.Available}, "
              + $"{owners.Value.Owners.Count} owners)"
            : $"FindResourceOwners : Unsupported — {owners.UnsupportedReason}");

        return 0;
    }
    catch (Exception ex)
    {
        Console.Error.WriteLine($"error: {ex.Message}");
        return 1;
    }
}

// Exercises the AtlasRules RPCs (rules CRUD + enable + simulate + interventions,
// and profiles CRUD + activate) and prints, for each, whether the server
// supported it or degraded to Unsupported. This is the console equivalent of
// navigating the UI's Rules / Profiles pages: against a server too old to serve
// AtlasRules (a NEW service that lands after this UI), every call should come
// back Unsupported (not throw), which is exactly what drives the pages'
// graceful "unavailable — server too old" states. Read-only calls are exercised
// first; the mutating probes only fire if the read side is already Supported, so
// this never writes rules into an older-but-partially-serving service.
static async Task<int> ProbeRulesAsync(string? who)
{
    try
    {
        using var atlas = AtlasChannel.Connect(who);
        Console.WriteLine("Probing AtlasRules RPCs ...");
        Console.WriteLine();

        var list = await atlas.ListRulesAsync();
        Console.WriteLine(list.Supported
            ? $"ListRules          : Supported ({list.Value.Rules.Count} rules)"
            : $"ListRules          : Unsupported — {list.UnsupportedReason}");

        var interventions = await atlas.ListInterventionsAsync();
        Console.WriteLine(interventions.Supported
            ? $"ListInterventions  : Supported ({interventions.Value.Interventions.Count} active)"
            : $"ListInterventions  : Unsupported — {interventions.UnsupportedReason}");

        // SimulateRule is a pure dry-run — safe to probe with a throwaway rule.
        var probeRule = new Atlas.V0.Rule
        {
            Name = "devcli probe",
            MatchImage = "explorer.exe",
            Trigger = Atlas.V0.RuleTrigger.WhileRunning,
            Action = new Atlas.V0.RuleAction
            {
                Priority = Atlas.V0.PriorityClass.PriorityBelowNormal,
                AffinityMode = Atlas.V0.CoreAffinityMode.PreferECores,
                EcoQos = true,
            },
        };
        var sim = await atlas.SimulateRuleAsync(probeRule);
        Console.WriteLine(sim.Supported
            ? $"SimulateRule       : Supported ({sim.Value.Targets.Count} targets, "
              + $"{sim.Value.Conflicts.Count} conflicts) — {RulesFormatter.SimulationSummary(sim.Value.Targets.Count, CountBlocked(sim.Value))}"
            : $"SimulateRule       : Unsupported — {sim.UnsupportedReason}");

        var profiles = await atlas.ListProfilesAsync();
        Console.WriteLine(profiles.Supported
            ? $"ListProfiles       : Supported ({profiles.Value.Profiles.Count} profiles)"
            : $"ListProfiles       : Unsupported — {profiles.UnsupportedReason}");

        Console.WriteLine();
        Console.WriteLine(list.Supported
            ? "AtlasRules is served — the Rules/Profiles pages will show live data."
            : "AtlasRules is not served — the Rules/Profiles pages will show their calm "
              + "\"unavailable — server too old\" states, and the app stays fully usable.");
        return 0;
    }
    catch (Exception ex)
    {
        Console.Error.WriteLine($"error: {ex.Message}");
        return 1;
    }
}

// Exercises the R2 advanced-privacy-alerts RPCs (ListPrivacyAlertRules,
// CreatePrivacyAlertRule, UpdatePrivacyAlertRule, DeletePrivacyAlertRule,
// ListFiredAlerts) and prints, for each, whether the server supported it or
// degraded to Unsupported. This is the console equivalent of navigating the UI's
// Privacy Alerts page: against a server too old to serve these RPCs, every call
// should come back Unsupported (not throw), which is exactly what drives the
// page's calm "unavailable — server too old" state. The read-only list calls are
// exercised first; the mutating create/update/delete probes only fire if the read
// side is already Supported, so this never writes rules into an older service.
static async Task<int> ProbePrivacyAlertsAsync(string? who)
{
    try
    {
        using var atlas = AtlasChannel.Connect(who);
        Console.WriteLine("Probing privacy-alert RPCs ...");
        Console.WriteLine();

        var rules = await atlas.ListPrivacyAlertRulesAsync();
        Console.WriteLine(rules.Supported
            ? $"ListPrivacyAlertRules : Supported ({rules.Value.Rules.Count} rules)"
            : $"ListPrivacyAlertRules : Unsupported — {rules.UnsupportedReason}");

        long now = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
        var fired = await atlas.ListFiredAlertsAsync(
            now - (long)TimeSpan.FromDays(7).TotalMilliseconds, now, limit: 50);
        Console.WriteLine(fired.Supported
            ? $"ListFiredAlerts       : Supported ({fired.Value.Alerts.Count} alerts, "
              + $"truncated={fired.Value.Truncated})"
            : $"ListFiredAlerts       : Unsupported — {fired.UnsupportedReason}");

        if (rules.Supported)
        {
            // The read side works, so the mutating RPCs are safe to exercise
            // (create a throwaway rule, toggle it, delete it) end to end.
            var draft = new Atlas.V0.PrivacyAlertRule
            {
                Name = "devcli probe",
                Enabled = false,
                Capability = Atlas.V0.CapabilityKind.Camera,
                Condition = Atlas.V0.PrivacyAlertCondition.AlertBackgroundUse,
            };
            var created = await atlas.CreatePrivacyAlertRuleAsync(draft);
            Console.WriteLine(created.Supported
                ? $"CreatePrivacyAlertRule: Supported (id={created.Value.Id})"
                : $"CreatePrivacyAlertRule: Unsupported — {created.UnsupportedReason}");

            if (created.Supported)
            {
                draft.Id = created.Value.Id;
                draft.Enabled = true;
                var updated = await atlas.UpdatePrivacyAlertRuleAsync(draft);
                Console.WriteLine(updated.Supported
                    ? $"UpdatePrivacyAlertRule: Supported (ok={updated.Value.Ok})"
                    : $"UpdatePrivacyAlertRule: Unsupported — {updated.UnsupportedReason}");

                var deleted = await atlas.DeletePrivacyAlertRuleAsync(created.Value.Id);
                Console.WriteLine(deleted.Supported
                    ? $"DeletePrivacyAlertRule: Supported (ok={deleted.Value.Ok})"
                    : $"DeletePrivacyAlertRule: Unsupported — {deleted.UnsupportedReason}");
            }
        }
        else
        {
            Console.WriteLine("CreatePrivacyAlertRule: skipped (read side Unsupported)");
            Console.WriteLine("UpdatePrivacyAlertRule: skipped (read side Unsupported)");
            Console.WriteLine("DeletePrivacyAlertRule: skipped (read side Unsupported)");
        }

        Console.WriteLine();
        Console.WriteLine(rules.Supported
            ? "Privacy alerts are served — the Privacy Alerts page will show live data."
            : "Privacy alerts are not served — the Privacy Alerts page will show its calm "
              + "\"unavailable — server too old\" state, and the app stays fully usable.");
        return 0;
    }
    catch (Exception ex)
    {
        Console.Error.WriteLine($"error: {ex.Message}");
        return 1;
    }
}

// Exercises the R3 remote support-bundle RPC (GenerateSupportBundle) and prints
// whether the server supported it or degraded to Unsupported. This is the console
// equivalent of opening the Settings page's "Create support bundle" dialog and
// clicking Generate: against a server too old to serve this RPC (the server side
// lands after this UI), the call should come back Unsupported (not throw), which
// is exactly what drives the dialog's calm "unavailable — server too old" state.
// When Supported, it also reports the redaction_applied echo the dialog shows the
// user. Read-only — a support bundle is assembled from data Atlas already has.
static async Task<int> ProbeSupportBundleAsync(string? who)
{
    try
    {
        using var atlas = AtlasChannel.Connect(who);
        Console.WriteLine("Probing GenerateSupportBundle ...");
        Console.WriteLine();

        long now = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
        long from = SupportBundleFormatter.WindowFromMs(now, 72);
        var redaction = new Atlas.V0.RedactionOptions
        {
            RedactUserNames = true,
            RedactComputerName = true,
            RedactPaths = true,
            RedactCommandLines = true,
        };

        var bundle = await atlas.GenerateSupportBundleAsync(
            from, now, Atlas.V0.ReportFormat.ReportHtml, redaction,
            SupportBundleFormatter.AllSections);

        Console.WriteLine(bundle.Supported
            ? $"GenerateSupportBundle : Supported (filename={bundle.Value.Filename}, "
              + $"{bundle.Value.Content.Length} chars, {bundle.Value.ContentType}) — "
              + SupportBundleFormatter.RedactionAppliedSummary(bundle.Value.RedactionApplied)
            : $"GenerateSupportBundle : Unsupported — {bundle.UnsupportedReason}");

        Console.WriteLine();
        Console.WriteLine(bundle.Supported
            ? "The support bundle is served — the Settings dialog will show a live preview."
            : "The support bundle is not served — the Settings dialog will show its calm "
              + "\"unavailable — server too old\" state, and the app stays fully usable.");
        return 0;
    }
    catch (Exception ex)
    {
        Console.Error.WriteLine($"error: {ex.Message}");
        return 1;
    }
}

// Exercises the R3 AtlasPlugins registry RPCs (ListPlugins, and — only when the
// read side is Supported — a careful RegisterPlugin → GrantPluginCapabilities →
// SetPluginEnabled → RemovePlugin roundtrip) and prints, for each, whether the
// server supported it or degraded to Unsupported. This is the console equivalent of
// navigating the UI's Plugins page: against a server too old to serve AtlasPlugins
// (a NEW service that lands after this UI), every call should come back Unsupported
// (not throw), which is exactly what drives the page's calm "unavailable — server
// too old" state. The mutating probes only fire if the read side is already
// Supported, so this never writes into an older-but-partially-serving service.
// OpenPluginSession is deliberately not exercised — that call belongs to a launched
// plugin process, never to the first-party UI/CLI.
static async Task<int> ProbePluginsAsync(string? who)
{
    try
    {
        using var atlas = AtlasChannel.Connect(who);
        Console.WriteLine("Probing AtlasPlugins RPCs ...");
        Console.WriteLine();

        var list = await atlas.ListPluginsAsync();
        Console.WriteLine(list.Supported
            ? $"ListPlugins             : Supported ({list.Value.Plugins.Count} plugins)"
            : $"ListPlugins             : Unsupported — {list.UnsupportedReason}");

        if (list.Supported)
        {
            // Read side works — exercise the mutating RPCs end to end with a
            // throwaway registration. A bogus path is expected to be refused
            // (ok=false) by a real server, which still proves the wrapper path.
            var caps = new[]
            {
                Atlas.V0.PluginCapability.PluginCapSnapshot,
                Atlas.V0.PluginCapability.PluginCapNetwork,
            };
            var registered = await atlas.RegisterPluginAsync(
                @"C:\Program Files\Atlas Plugins\devcli-probe\devcli-probe.exe", caps, allowUnsigned: false);
            Console.WriteLine(registered.Supported
                ? $"RegisterPlugin          : Supported (ok={registered.Value.Ok}, msg=\"{registered.Value.Message}\")"
                : $"RegisterPlugin          : Unsupported — {registered.UnsupportedReason}");

            if (registered.Supported && registered.Value.Ok && registered.Value.Plugin is not null)
            {
                long id = registered.Value.Plugin.Id;

                var granted = await atlas.GrantPluginCapabilitiesAsync(
                    id, new[] { Atlas.V0.PluginCapability.PluginCapSnapshot });
                Console.WriteLine(granted.Supported
                    ? $"GrantPluginCapabilities : Supported (ok={granted.Value.Ok})"
                    : $"GrantPluginCapabilities : Unsupported — {granted.UnsupportedReason}");

                var enabled = await atlas.SetPluginEnabledAsync(id, true);
                Console.WriteLine(enabled.Supported
                    ? $"SetPluginEnabled        : Supported (ok={enabled.Value.Ok})"
                    : $"SetPluginEnabled        : Unsupported — {enabled.UnsupportedReason}");

                var removed = await atlas.RemovePluginAsync(id);
                Console.WriteLine(removed.Supported
                    ? $"RemovePlugin            : Supported (ok={removed.Value.Ok})"
                    : $"RemovePlugin            : Unsupported — {removed.UnsupportedReason}");
            }
            else
            {
                Console.WriteLine("GrantPluginCapabilities : skipped (registration did not create a plugin)");
                Console.WriteLine("SetPluginEnabled        : skipped (registration did not create a plugin)");
                Console.WriteLine("RemovePlugin            : skipped (registration did not create a plugin)");
            }
        }
        else
        {
            Console.WriteLine("RegisterPlugin          : skipped (read side Unsupported)");
            Console.WriteLine("GrantPluginCapabilities : skipped (read side Unsupported)");
            Console.WriteLine("SetPluginEnabled        : skipped (read side Unsupported)");
            Console.WriteLine("RemovePlugin            : skipped (read side Unsupported)");
        }

        Console.WriteLine();
        Console.WriteLine(list.Supported
            ? "AtlasPlugins is served — the Plugins page will show live data."
            : "AtlasPlugins is not served — the Plugins page will show its calm "
              + "\"unavailable — server too old\" state, and the app stays fully usable.");
        return 0;
    }
    catch (Exception ex)
    {
        Console.Error.WriteLine($"error: {ex.Message}");
        return 1;
    }
}

static int CountBlocked(Atlas.V0.SimulateRuleReply reply)
{
    int blocked = 0;
    foreach (var t in reply.Targets)
    {
        if (t.Blocked)
        {
            blocked++;
        }
    }
    return blocked;
}
