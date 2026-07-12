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
        case "-h":
        case "--help":
            Console.WriteLine("usage: atlas-devcli [--pipe <who>] [--top <n>] [--watch]");
            return 0;
        default:
            Console.Error.WriteLine($"unknown argument: {args[i]}");
            return 2;
    }
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
