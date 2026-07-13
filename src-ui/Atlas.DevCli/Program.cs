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
                "usage: atlas-devcli [--pipe <who>] [--top <n>] [--watch] [--probe-r2 [--pid <pid>]]");
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
