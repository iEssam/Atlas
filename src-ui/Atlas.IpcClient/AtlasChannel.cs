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
    private readonly AtlasControl.AtlasControlClient _control;

    private AtlasChannel(GrpcChannel channel)
    {
        _channel = channel;
        _client = new AtlasQuery.AtlasQueryClient(channel);
        _control = new AtlasControl.AtlasControlClient(channel);
    }

    /// <summary>
    /// Runs a unary call, mapping <c>StatusCode.Unimplemented</c> to a typed
    /// <see cref="RpcOutcome{T}.Unsupported"/> result so callers can degrade
    /// gracefully against an older server. Other faults propagate unchanged.
    /// </summary>
    private static async Task<RpcOutcome<T>> GuardAsync<T>(Func<Task<T>> call)
    {
        try
        {
            return RpcOutcome<T>.Ok(await call().ConfigureAwait(false));
        }
        catch (RpcException ex) when (ex.StatusCode == StatusCode.Unimplemented)
        {
            return RpcOutcome<T>.Unsupported(
                string.IsNullOrEmpty(ex.Status.Detail) ? "not implemented" : ex.Status.Detail);
        }
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

    // ----------------------------------------------------------------------
    // M6: historical queries, search, bookmarks (AtlasQuery). Each wraps the
    // Unimplemented status into a typed "not supported" outcome so the new
    // pages degrade gracefully against an older service (task brief).
    // ----------------------------------------------------------------------

    /// <summary>
    /// Decimated min/max/avg buckets for one metric over a window. The server
    /// never returns more than <paramref name="buckets"/> buckets (0 = server
    /// default); empty buckets are omitted so gaps render as missing data.
    /// </summary>
    public Task<RpcOutcome<QueryRangeReply>> QueryRangeAsync(
        MetricKind metric,
        long scope,
        long fromMs,
        long toMs,
        uint buckets = 0,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _client.QueryRangeAsync(
            new QueryRangeRequest
            {
                Metric = metric,
                Scope = scope,
                Range = new TimeRange { FromMs = fromMs, ToMs = toMs },
                Buckets = buckets,
            },
            cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>Process start/stop events in a window (empty kinds = all).</summary>
    public Task<RpcOutcome<ListEventsReply>> ListEventsAsync(
        long fromMs,
        long toMs,
        uint limit = 0,
        IEnumerable<uint>? kinds = null,
        CancellationToken cancellationToken = default)
    {
        var req = new ListEventsRequest
        {
            Range = new TimeRange { FromMs = fromMs, ToMs = toMs },
            Limit = limit,
        };
        if (kinds is not null)
        {
            req.Kinds.AddRange(kinds);
        }
        return GuardAsync(() => _client.ListEventsAsync(
            req, cancellationToken: cancellationToken).ResponseAsync);
    }

    /// <summary>Global search across processes, events, and bookmarks.</summary>
    public Task<RpcOutcome<SearchReply>> SearchAsync(
        string query,
        uint limit = 0,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _client.SearchAsync(
            new SearchRequest { Query = query, Limit = limit },
            cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>Creates an incident bookmark at <paramref name="tsMs"/>.</summary>
    public Task<RpcOutcome<CreateBookmarkReply>> CreateBookmarkAsync(
        long tsMs,
        string label,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _client.CreateBookmarkAsync(
            new CreateBookmarkRequest { TsMs = tsMs, Label = label },
            cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>Bookmarks falling within a window.</summary>
    public Task<RpcOutcome<ListBookmarksReply>> ListBookmarksAsync(
        long fromMs,
        long toMs,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _client.ListBookmarksAsync(
            new ListBookmarksRequest { Range = new TimeRange { FromMs = fromMs, ToMs = toMs } },
            cancellationToken: cancellationToken).ResponseAsync);

    // ----------------------------------------------------------------------
    // M7: privacy activity, startup inventory, services (AtlasQuery). Same
    // Unimplemented→Unsupported guard so the new pages degrade gracefully
    // against an older service that doesn't serve these RPCs yet (task brief).
    // ----------------------------------------------------------------------

    /// <summary>
    /// Current per-(app, capability) privacy usage from the ConsentStore. Pass
    /// specific <paramref name="capabilities"/> to filter, or none for all.
    /// </summary>
    public Task<RpcOutcome<ListPrivacyUsageReply>> ListPrivacyUsageAsync(
        IEnumerable<CapabilityKind>? capabilities = null,
        CancellationToken cancellationToken = default)
    {
        var req = new ListPrivacyUsageRequest();
        if (capabilities is not null)
        {
            req.Capabilities.AddRange(capabilities);
        }
        return GuardAsync(() => _client.ListPrivacyUsageAsync(
            req, cancellationToken: cancellationToken).ResponseAsync);
    }

    /// <summary>Recent privacy start/stop transitions in a window.</summary>
    public Task<RpcOutcome<ListPrivacyEventsReply>> ListPrivacyEventsAsync(
        long fromMs,
        long toMs,
        uint limit = 0,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _client.ListPrivacyEventsAsync(
            new ListPrivacyEventsRequest
            {
                Range = new TimeRange { FromMs = fromMs, ToMs = toMs },
                Limit = limit,
            },
            cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>The startup inventory (run keys, folders, tasks, services, packaged).</summary>
    public Task<RpcOutcome<ListStartupReply>> ListStartupAsync(
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _client.ListStartupAsync(
            new ListStartupRequest(), cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>
    /// The service inventory. <paramref name="filter"/> is a case-insensitive
    /// substring over name/display_name (empty = all).
    /// </summary>
    public Task<RpcOutcome<ListServicesReply>> ListServicesAsync(
        string filter = "",
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _client.ListServicesAsync(
            new ListServicesRequest { Filter = filter ?? string.Empty },
            cancellationToken: cancellationToken).ResponseAsync);

    // ----------------------------------------------------------------------
    // M8: incident detection, diagnostics, reports (AtlasQuery, PRD §9.15,
    // §9.18). Same Unimplemented→Unsupported guard so the Diagnostics page
    // degrades gracefully against an older service that serves these RPCs as
    // Unimplemented (task brief — the server side lands after the UI).
    // ----------------------------------------------------------------------

    /// <summary>
    /// Detected incidents overlapping a window, most-relevant first (server
    /// order). <paramref name="limit"/> of 0 uses the server default. The reply's
    /// <c>truncated</c> flag says whether more were elided.
    /// </summary>
    public Task<RpcOutcome<ListIncidentsReply>> ListIncidentsAsync(
        long fromMs,
        long toMs,
        uint limit = 0,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _client.ListIncidentsAsync(
            new ListIncidentsRequest
            {
                Range = new TimeRange { FromMs = fromMs, ToMs = toMs },
                Limit = limit,
            },
            cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>
    /// Diagnoses a detected incident by id, or — when <paramref name="incidentId"/>
    /// is 0 — the ad-hoc [<paramref name="fromMs"/>, <paramref name="toMs"/>]
    /// window. The reply may say <c>available = false</c> with a plain reason
    /// ("insufficient evidence for this window"); callers must surface that rather
    /// than invent a diagnosis (PRD §9.16.4).
    /// </summary>
    public Task<RpcOutcome<DiagnoseReply>> DiagnoseAsync(
        long incidentId,
        long fromMs,
        long toMs,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _client.DiagnoseAsync(
            new DiagnoseRequest
            {
                IncidentId = incidentId,
                Range = new TimeRange { FromMs = fromMs, ToMs = toMs },
            },
            cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>
    /// Renders a report for an incident (by id) or the ad-hoc window (id 0) in the
    /// requested <paramref name="format"/>, applying <paramref name="redaction"/>
    /// server-side. Returns the content plus its MIME type.
    /// </summary>
    public Task<RpcOutcome<GenerateReportReply>> GenerateReportAsync(
        long incidentId,
        long fromMs,
        long toMs,
        ReportFormat format,
        RedactionOptions redaction,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _client.GenerateReportAsync(
            new GenerateReportRequest
            {
                IncidentId = incidentId,
                Range = new TimeRange { FromMs = fromMs, ToMs = toMs },
                Format = format,
                Redaction = redaction ?? new RedactionOptions(),
            },
            cancellationToken: cancellationToken).ResponseAsync);

    // ----------------------------------------------------------------------
    // R2: deep process inspector + resource ownership (AtlasQuery, PRD §9.4,
    // §9.5). All on-demand, read-only. Same Unimplemented→Unsupported guard so
    // the Inspector and File-Lock pages degrade gracefully against an older
    // service that serves these RPCs as Unimplemented (the server side lands
    // after the UI — task brief). Note these replies also carry their <em>own</em>
    // in-band coverage flags (limited / names_limited / available) which callers
    // surface honestly; that is orthogonal to the transport-level Unsupported.
    // ----------------------------------------------------------------------

    /// <summary>
    /// Full detail for one process, identified by <paramref name="pid"/> and
    /// guarded against PID reuse by <paramref name="createTime100ns"/> (0 =
    /// best-effort by pid). The reply may report <c>available = false</c> ("process
    /// exited" / "access denied") or <c>limited = true</c> (some fields needed
    /// elevation); callers surface both rather than inventing data.
    /// </summary>
    public Task<RpcOutcome<ProcessDetailReply>> GetProcessDetailAsync(
        uint pid,
        long createTime100ns,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _client.GetProcessDetailAsync(
            new ProcessDetailRequest { Pid = pid, CreateTime100Ns = createTime100ns },
            cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>
    /// A process's open handles. <paramref name="typeFilter"/> is an exact object
    /// type ("File", "Key", "Event", …); empty returns all types.
    /// <paramref name="limit"/> of 0 uses the server default. The reply's
    /// <c>truncated</c> flag says whether more were elided and <c>names_limited</c>
    /// whether some names couldn't be resolved without elevation.
    /// </summary>
    public Task<RpcOutcome<ListHandlesReply>> ListHandlesAsync(
        uint pid,
        string typeFilter = "",
        uint limit = 0,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _client.ListHandlesAsync(
            new ListHandlesRequest
            {
                Pid = pid,
                TypeFilter = typeFilter ?? string.Empty,
                Limit = limit,
            },
            cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>
    /// A process's loaded modules (DLLs/images). The reply may report
    /// <c>available = false</c> ("access denied (elevation may help)") for a
    /// cross-user process; callers surface that instead of an empty list.
    /// </summary>
    public Task<RpcOutcome<ListModulesReply>> ListModulesAsync(
        uint pid,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _client.ListModulesAsync(
            new ListModulesRequest { Pid = pid },
            cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>A process's threads (tid, start address, state, priority, times).</summary>
    public Task<RpcOutcome<ListThreadsReply>> ListThreadsAsync(
        uint pid,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _client.ListThreadsAsync(
            new ListThreadsRequest { Pid = pid },
            cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>
    /// The processes currently holding <paramref name="path"/> open (PRD §9.5,
    /// "find what is using this file", Restart Manager first). The reply
    /// distinguishes <c>available = false</c> (path not found / access denied,
    /// with a reason) from an empty owner list (nothing is holding the file).
    /// </summary>
    public Task<RpcOutcome<FindResourceOwnersReply>> FindResourceOwnersAsync(
        string path,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _client.FindResourceOwnersAsync(
            new FindResourceOwnersRequest { Path = path ?? string.Empty },
            cancellationToken: cancellationToken).ResponseAsync);

    // ----------------------------------------------------------------------
    // M6: safe process actions (AtlasControl) — two-phase prepare/execute.
    // ----------------------------------------------------------------------

    /// <summary>
    /// Phase 1: asks the broker to assess an action against a process. Returns
    /// the risk picture plus a short-lived, single-use consent token when
    /// allowed. PID reuse is guarded by <paramref name="createTime100ns"/>.
    /// </summary>
    public Task<RpcOutcome<PrepareActionReply>> PrepareActionAsync(
        uint pid,
        long createTime100ns,
        ProcessActionKind action,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _control.PrepareActionAsync(
            new PrepareActionRequest
            {
                Pid = pid,
                CreateTime100Ns = createTime100ns,
                Action = action,
            },
            cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>
    /// Phase 2: executes exactly the prepared action, identified by the opaque
    /// single-use <paramref name="consentToken"/> from
    /// <see cref="PrepareActionAsync"/>.
    /// </summary>
    public Task<RpcOutcome<ExecuteActionReply>> ExecuteActionAsync(
        string consentToken,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _control.ExecuteActionAsync(
            new ExecuteActionRequest { ConsentToken = consentToken },
            cancellationToken: cancellationToken).ResponseAsync);

    public void Dispose() => _channel.Dispose();
}
