using System.IO.Pipes;
using System.Net.Sockets;
using Atlas.V0;
using Grpc.Core;
using Grpc.Net.Client;

namespace Atlas.IpcClient;

/// <summary>
/// A gRPC client to the Atlas service that dials the Rust host's Windows named
/// pipe. tonic serves gRPC (HTTP/2) over the raw pipe byte stream; on the .NET
/// side we hand <see cref="GrpcChannel"/> a <see cref="SocketsHttpHandler"/>
/// whose <see cref="SocketsHttpHandler.ConnectCallback"/> opens a
/// <see cref="NamedPipeClientStream"/> instead of a TCP socket (tech-stack §5).
/// </summary>
public sealed class AtlasChannel : IDisposable
{
    private readonly GrpcChannel _channel;
    private readonly AtlasQuery.AtlasQueryClient _client;

    private AtlasChannel(GrpcChannel channel)
    {
        _channel = channel;
        _client = new AtlasQuery.AtlasQueryClient(channel);
    }

    /// <summary>
    /// Builds a channel for the given <paramref name="who"/> discriminator
    /// (e.g. <c>uidev</c> for <c>serve --pipe uidev</c>). Pass <c>null</c> to
    /// use the same default the Rust side uses (the <c>USERNAME</c> env var).
    /// </summary>
    public static AtlasChannel Connect(string? who = null)
    {
        var pipeName = AtlasPipe.PipeName(who ?? AtlasPipe.DefaultWho());
        return ConnectToPipe(pipeName);
    }

    /// <summary>
    /// Builds a channel against an explicit pipe name (the portion after
    /// <c>\\.\pipe\</c>). Mainly for tests / advanced callers.
    /// </summary>
    public static AtlasChannel ConnectToPipe(string pipeName)
    {
        var handler = new SocketsHttpHandler
        {
            // Named pipes have no host; each HTTP/2 connection request dials a
            // fresh pipe client. Pooling is meaningless over a single-instance
            // pipe hand-off, so keep connections eager and simple.
            ConnectCallback = async (ctx, cancellationToken) =>
            {
                var stream = new NamedPipeClientStream(
                    serverName: ".",
                    pipeName: pipeName,
                    direction: PipeDirection.InOut,
                    options: PipeOptions.Asynchronous | PipeOptions.WriteThrough);

                // The Rust accept loop always keeps one instance waiting, but
                // there is still a small connect race during server startup /
                // instance hand-off. A bounded wait mirrors the Rust `dial`
                // retry (ERROR_PIPE_BUSY / ERROR_FILE_NOT_FOUND).
                await stream.ConnectAsync(TimeSpan.FromSeconds(5), cancellationToken)
                    .ConfigureAwait(false);
                return stream;
            },
            EnableMultipleHttp2Connections = true,
        };

        var channel = GrpcChannel.ForAddress(
            // Authority is a placeholder; tonic/hyper never resolves it because
            // the ConnectCallback supplies the transport. Must be a valid URI.
            "http://atlas.local",
            new GrpcChannelOptions
            {
                HttpHandler = handler,
                // Explicit: our transport is the pipe, not TLS.
                Credentials = ChannelCredentials.Insecure,
            });

        return new AtlasChannel(channel);
    }

    /// <summary>Fetches the service version and capability flags.</summary>
    public async Task<CapabilitiesReply> GetCapabilitiesAsync(
        CancellationToken cancellationToken = default)
    {
        return await _client
            .GetCapabilitiesAsync(new CapabilitiesRequest(), cancellationToken: cancellationToken)
            .ConfigureAwait(false);
    }

    /// <summary>
    /// Fetches one snapshot. <paramref name="topN"/> of 0 returns all
    /// processes; otherwise the top-N by CPU (already sorted server-side).
    /// </summary>
    public async Task<SnapshotReply> GetSnapshotAsync(
        uint topN = 0,
        CancellationToken cancellationToken = default)
    {
        return await _client
            .GetSnapshotAsync(new SnapshotRequest { TopN = topN }, cancellationToken: cancellationToken)
            .ConfigureAwait(false);
    }

    /// <summary>
    /// Server-streams snapshots (~1 Hz). The first item arrives immediately
    /// (the server emits the current snapshot on subscribe). Enumeration ends
    /// when the token is cancelled or the server closes the stream.
    /// </summary>
    public async IAsyncEnumerable<SnapshotReply> StreamSnapshotsAsync(
        uint topN = 0,
        [System.Runtime.CompilerServices.EnumeratorCancellation]
        CancellationToken cancellationToken = default)
    {
        using var call = _client.StreamSnapshots(
            new SnapshotRequest { TopN = topN },
            cancellationToken: cancellationToken);

        await foreach (var reply in call.ResponseStream
            .ReadAllAsync(cancellationToken)
            .ConfigureAwait(false))
        {
            yield return reply;
        }
    }

    public void Dispose() => _channel.Dispose();
}
