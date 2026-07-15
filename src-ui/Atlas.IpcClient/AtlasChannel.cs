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
    private readonly AtlasRules.AtlasRulesClient _rules;
    private readonly AtlasPlugins.AtlasPluginsClient _plugins;

    private AtlasChannel(GrpcChannel channel)
    {
        _channel = channel;
        _client = new AtlasQuery.AtlasQueryClient(channel);
        _control = new AtlasControl.AtlasControlClient(channel);
        _rules = new AtlasRules.AtlasRulesClient(channel);
        _plugins = new AtlasPlugins.AtlasPluginsClient(channel);
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
    // R2: monitors — network, scheduled tasks, boot analysis, battery, thermal
    // (AtlasQuery, PRD §9.12, §9.9.2, §9.6.6, §9.6.7). All read-only. Same
    // Unimplemented→Unsupported guard so the new pages/cards degrade gracefully
    // against an older service that serves these RPCs as Unimplemented (the
    // server side lands after the UI — task brief). The hardware-dependent
    // replies (boots / battery / thermal) additionally carry their own in-band
    // <c>available</c> + <c>unavailable_reason</c>, which callers surface
    // honestly (absent sensors are information, not an error); that is orthogonal
    // to the transport-level Unsupported.
    // ----------------------------------------------------------------------

    /// <summary>
    /// Active network connections (per PID). Set <paramref name="includeListening"/>
    /// to also fold in listening/bound endpoints; otherwise only established/active
    /// connections are returned. Remote domains are populated from the DNS cache
    /// where available and left empty otherwise.
    /// </summary>
    public Task<RpcOutcome<ListConnectionsReply>> ListConnectionsAsync(
        bool includeListening = false,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _client.ListConnectionsAsync(
            new ListConnectionsRequest { IncludeListening = includeListening },
            cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>
    /// The listening/bound TCP and UDP ports with their owning process. The
    /// server-side complement of the Network page's "what is listening?" view.
    /// </summary>
    public Task<RpcOutcome<ListListeningPortsReply>> ListListeningPortsAsync(
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _client.ListListeningPortsAsync(
            new ListListeningPortsRequest(),
            cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>
    /// The Windows scheduled-task inventory. <paramref name="filter"/> is a
    /// case-insensitive substring over name/path (empty = all). Read-only.
    /// </summary>
    public Task<RpcOutcome<ListScheduledTasksReply>> ListScheduledTasksAsync(
        string filter = "",
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _client.ListScheduledTasksAsync(
            new ListScheduledTasksRequest { Filter = filter ?? string.Empty },
            cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>
    /// Recent boot records (duration + degraded flag), newest first.
    /// <paramref name="limit"/> of 0 uses the server default. The reply may report
    /// <c>available = false</c> ("diagnostics-performance log unavailable") — a
    /// plain, expected state on machines where the log is off, not an error.
    /// </summary>
    public Task<RpcOutcome<ListBootsReply>> ListBootsAsync(
        uint limit = 0,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _client.ListBootsAsync(
            new ListBootsRequest { Limit = limit },
            cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>
    /// The battery status. On a desktop the reply reports <c>available = false</c>
    /// with "no battery present" — a calm, expected fact the UI states plainly, not
    /// an error.
    /// </summary>
    public Task<RpcOutcome<GetBatteryStatusReply>> GetBatteryStatusAsync(
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _client.GetBatteryStatusAsync(
            new GetBatteryStatusRequest(),
            cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>
    /// Per-sensor temperatures. Many machines expose none; the reply then reports
    /// <c>available = false</c> ("no thermal sensors exposed") — the honest state,
    /// surfaced without implying a problem (task brief §4).
    /// </summary>
    public Task<RpcOutcome<GetThermalReply>> GetThermalAsync(
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _client.GetThermalAsync(
            new GetThermalRequest(),
            cancellationToken: cancellationToken).ResponseAsync);

    // ----------------------------------------------------------------------
    // R2: advanced privacy alerts (AtlasQuery, PRD §9.10.3). Alert rules over
    // camera / microphone / location usage, plus the fired-alert log. All
    // read-only from the UI's perspective except the rule CRUD, which the
    // service's ConsentStore change-watcher evaluates. Same Unimplemented→
    // Unsupported guard so the privacy-alerts page degrades gracefully against an
    // older service that serves these RPCs as Unimplemented (the server side
    // lands after the UI — task brief). A fired alert means "you asked to be told
    // about this", never "a threat" — the framing stays factual (proto R2 header).
    // ----------------------------------------------------------------------

    /// <summary>All configured privacy-alert rules (enabled and disabled), in server order.</summary>
    public Task<RpcOutcome<ListPrivacyAlertRulesReply>> ListPrivacyAlertRulesAsync(
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _client.ListPrivacyAlertRulesAsync(
            new ListPrivacyAlertRulesRequest(),
            cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>
    /// Creates a privacy-alert rule (the <c>id</c> field is ignored server-side).
    /// Returns the assigned id. A disabled rule watches nothing until enabled.
    /// </summary>
    public Task<RpcOutcome<CreatePrivacyAlertRuleReply>> CreatePrivacyAlertRuleAsync(
        PrivacyAlertRule rule,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _client.CreatePrivacyAlertRuleAsync(
            new CreatePrivacyAlertRuleRequest { Rule = rule },
            cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>
    /// Updates an existing privacy-alert rule in place (matched by its <c>id</c>).
    /// The reply's <c>ok</c> flag carries the server-side result.
    /// </summary>
    public Task<RpcOutcome<UpdatePrivacyAlertRuleReply>> UpdatePrivacyAlertRuleAsync(
        PrivacyAlertRule rule,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _client.UpdatePrivacyAlertRuleAsync(
            new UpdatePrivacyAlertRuleRequest { Rule = rule },
            cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>Deletes a privacy-alert rule by id. The rule simply stops watching.</summary>
    public Task<RpcOutcome<DeletePrivacyAlertRuleReply>> DeletePrivacyAlertRuleAsync(
        long id,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _client.DeletePrivacyAlertRuleAsync(
            new DeletePrivacyAlertRuleRequest { Id = id },
            cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>
    /// Alerts that have fired in a window, newest first. <paramref name="limit"/>
    /// of 0 uses the server default; the reply's <c>truncated</c> flag says whether
    /// more were elided. A fired alert is an informational record ("you asked to be
    /// told about this"), never a verdict.
    /// </summary>
    public Task<RpcOutcome<ListFiredAlertsReply>> ListFiredAlertsAsync(
        long fromMs,
        long toMs,
        uint limit = 0,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _client.ListFiredAlertsAsync(
            new ListFiredAlertsRequest
            {
                Range = new TimeRange { FromMs = fromMs, ToMs = toMs },
                Limit = limit,
            },
            cancellationToken: cancellationToken).ResponseAsync);

    // ----------------------------------------------------------------------
    // R3: system-change tracking + reliability/crash correlation (AtlasQuery,
    // PRD §9.13, §9.14). Both read-only. Same Unimplemented→Unsupported guard so
    // the System Changes and Reliability pages degrade gracefully against an older
    // service that serves these RPCs as Unimplemented (the server side lands after
    // the UI — task brief). ListCrashes additionally carries its own in-band
    // <c>available</c> + <c>unavailable_reason</c> (e.g. "reliability log
    // unavailable"), which the page surfaces honestly; that is orthogonal to the
    // transport-level Unsupported. A change is information and a crash record is
    // history + context — never an accusation (proto R3 header).
    // ----------------------------------------------------------------------

    /// <summary>
    /// System changes recorded over a window (newest first, server order): app /
    /// driver / update / service / startup / task / power / default-app changes.
    /// Pass specific <paramref name="kinds"/> to filter, or none for all.
    /// <paramref name="limit"/> of 0 uses the server default; the reply's
    /// <c>truncated</c> flag says whether more were elided. Read-only — a change is
    /// a fact about what happened, not a verdict.
    /// </summary>
    public Task<RpcOutcome<ListSystemChangesReply>> ListSystemChangesAsync(
        long fromMs,
        long toMs,
        IEnumerable<SystemChangeKind>? kinds = null,
        uint limit = 0,
        CancellationToken cancellationToken = default)
    {
        var req = new ListSystemChangesRequest
        {
            Range = new TimeRange { FromMs = fromMs, ToMs = toMs },
            Limit = limit,
        };
        if (kinds is not null)
        {
            req.Kinds.AddRange(kinds);
        }
        return GuardAsync(() => _client.ListSystemChangesAsync(
            req, cancellationToken: cancellationToken).ResponseAsync);
    }

    /// <summary>
    /// Crash / hang / bugcheck / service-failure / unexpected-shutdown records over
    /// a window, each with its correlated context. Pass specific
    /// <paramref name="kinds"/> to filter, or none for all. The reply may report
    /// <c>available = false</c> with a plain reason ("reliability log unavailable")
    /// — a calm, expected state on machines where the log is off, which callers
    /// surface rather than an empty list; the <c>truncated</c> flag says whether
    /// more were elided. Read-only — a record is history and context, not blame.
    /// </summary>
    public Task<RpcOutcome<ListCrashesReply>> ListCrashesAsync(
        long fromMs,
        long toMs,
        IEnumerable<CrashKind>? kinds = null,
        uint limit = 0,
        CancellationToken cancellationToken = default)
    {
        var req = new ListCrashesRequest
        {
            Range = new TimeRange { FromMs = fromMs, ToMs = toMs },
            Limit = limit,
        };
        if (kinds is not null)
        {
            req.Kinds.AddRange(kinds);
        }
        return GuardAsync(() => _client.ListCrashesAsync(
            req, cancellationToken: cancellationToken).ResponseAsync);
    }

    // ----------------------------------------------------------------------
    // R3: remote support bundle (AtlasQuery, PRD §9.18, §18.3). One redacted,
    // self-contained diagnostic document the user can hand to IT/support,
    // assembled from data Atlas already has and passed through the shared
    // Redactor. Read-only. Same Unimplemented→Unsupported guard so the export
    // dialog shows a calm "unavailable" state against an older service that
    // serves this RPC as Unimplemented (the server side lands after the UI —
    // task brief). The reply echoes back the redaction categories actually
    // applied so the UI can show the user exactly what was stripped.
    // ----------------------------------------------------------------------

    /// <summary>
    /// Generates a remote support bundle over <paramref name="fromMs"/>..<paramref name="toMs"/>
    /// (the window bounds the incident/change/crash sections) in the requested
    /// <paramref name="format"/>, including the given <paramref name="sections"/>
    /// (empty = all) and applying <paramref name="redaction"/> server-side.
    /// Returns the rendered content, its MIME type, a suggested filename, and the
    /// echo of the redaction categories actually applied.
    /// </summary>
    public Task<RpcOutcome<SupportBundleReply>> GenerateSupportBundleAsync(
        long fromMs,
        long toMs,
        ReportFormat format,
        RedactionOptions redaction,
        IEnumerable<SupportBundleSection>? sections = null,
        CancellationToken cancellationToken = default)
    {
        var req = new SupportBundleRequest
        {
            Range = new TimeRange { FromMs = fromMs, ToMs = toMs },
            Format = format,
            Redaction = redaction ?? new RedactionOptions(),
        };
        if (sections is not null)
        {
            req.Sections.AddRange(sections);
        }
        return GuardAsync(() => _client.GenerateSupportBundleAsync(
            req, cancellationToken: cancellationToken).ResponseAsync);
    }

    // ----------------------------------------------------------------------
    // R3: expert security metadata (AtlasQuery, PRD §9.4.1/§9.4.6). One
    // on-demand, read-only RPC feeding the Inspector's Security tab — the signing
    // certificate chain, file hash, token privileges/groups/capabilities, and
    // process mitigation policies. Same Unimplemented→Unsupported guard so the
    // Security tab shows a calm "server too old" state against an older service
    // that serves this RPC as Unimplemented (the server side lands after the UI —
    // task brief). The reply also carries its own in-band coverage flags
    // (available / unavailable_reason, and metadata.limited) which the tab surfaces
    // honestly — a blank field or an unsigned binary is information, never an
    // accusation; that is orthogonal to the transport-level Unsupported.
    // ----------------------------------------------------------------------

    /// <summary>
    /// Deep security metadata for one process, identified by <paramref name="pid"/>
    /// and guarded against PID reuse by <paramref name="createTime100ns"/> (0 =
    /// best-effort by pid). The reply may report <c>available = false</c> ("process
    /// exited" / "access denied") or set <c>metadata.limited</c> (some fields needed
    /// elevation); callers surface both honestly rather than inventing data.
    /// </summary>
    public Task<RpcOutcome<GetSecurityMetadataReply>> GetSecurityMetadataAsync(
        uint pid,
        long createTime100ns,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _client.GetSecurityMetadataAsync(
            new GetSecurityMetadataRequest { Pid = pid, CreateTime100Ns = createTime100ns },
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

    // ----------------------------------------------------------------------
    // R2: rules engine + profiles (AtlasRules, PRD §9.7). A NEW service — its
    // own client alongside AtlasQuery/AtlasControl. Enabling a rule IS the
    // consent; every application is reversible and audited, and
    // protected-critical processes are never touched (proto R2 header). Same
    // Unimplemented→Unsupported guard so the Rules and Profiles pages degrade
    // gracefully against an older service that doesn't serve AtlasRules yet: the
    // server side lands after the UI (task brief). SimulateRule is a pure
    // dry-run — it never applies anything.
    // ----------------------------------------------------------------------

    /// <summary>All configured rules (enabled and disabled), in server order.</summary>
    public Task<RpcOutcome<ListRulesReply>> ListRulesAsync(
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _rules.ListRulesAsync(
            new ListRulesRequest(), cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>
    /// One rule by id. The reply's <c>found</c> flag distinguishes "no such rule"
    /// from a transport failure; callers check it before reading <c>rule</c>.
    /// </summary>
    public Task<RpcOutcome<GetRuleReply>> GetRuleAsync(
        long id,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _rules.GetRuleAsync(
            new GetRuleRequest { Id = id }, cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>
    /// Creates a rule (the <c>id</c> field is ignored server-side). Returns the
    /// assigned id. A newly created rule is applied only if its <c>enabled</c>
    /// flag is set — creating a disabled rule changes nothing on the system.
    /// </summary>
    public Task<RpcOutcome<CreateRuleReply>> CreateRuleAsync(
        Rule rule,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _rules.CreateRuleAsync(
            new CreateRuleRequest { Rule = rule }, cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>
    /// Updates an existing rule in place (matched by its <c>id</c>). The reply's
    /// <c>ok</c>/<c>message</c> carry a server-side validation result.
    /// </summary>
    public Task<RpcOutcome<UpdateRuleReply>> UpdateRuleAsync(
        Rule rule,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _rules.UpdateRuleAsync(
            new UpdateRuleRequest { Rule = rule }, cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>
    /// Deletes a rule by id. Deleting a rule reverts anything it was applying
    /// (the engine restores affected processes), so this is safe and reversible in
    /// effect even though the rule row itself is gone.
    /// </summary>
    public Task<RpcOutcome<DeleteRuleReply>> DeleteRuleAsync(
        long id,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _rules.DeleteRuleAsync(
            new DeleteRuleRequest { Id = id }, cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>
    /// Toggles a rule's enabled state. Enabling applies it to matching processes;
    /// disabling reverts its interventions. This IS the consent gesture for a
    /// persistent policy (proto R2 header).
    /// </summary>
    public Task<RpcOutcome<SetRuleEnabledReply>> SetRuleEnabledAsync(
        long id,
        bool enabled,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _rules.SetRuleEnabledAsync(
            new SetRuleEnabledRequest { Id = id, Enabled = enabled },
            cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>
    /// Dry-run preview of a rule (PRD §9.7.5) — <b>never applies anything</b>.
    /// The (possibly unsaved) <paramref name="rule"/> is matched against currently
    /// running processes; the reply lists each affected target with its current→new
    /// priority/affinity and eco change, marks protected-critical targets as
    /// <c>blocked</c>, and reports any conflicts with other enabled rules. This is
    /// the centerpiece the user sees before committing.
    /// </summary>
    public Task<RpcOutcome<SimulateRuleReply>> SimulateRuleAsync(
        Rule rule,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _rules.SimulateRuleAsync(
            new SimulateRuleRequest { Rule = rule }, cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>
    /// The interventions Atlas is currently applying — what, to which process, by
    /// which rule, and since when (PRD §9.7.3). The transparency surface: the user
    /// always sees exactly what Atlas is doing to their system.
    /// </summary>
    public Task<RpcOutcome<ListInterventionsReply>> ListInterventionsAsync(
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _rules.ListInterventionsAsync(
            new ListInterventionsRequest(), cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>All profiles (activatable bundles of rules + a power mode).</summary>
    public Task<RpcOutcome<ListProfilesReply>> ListProfilesAsync(
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _rules.ListProfilesAsync(
            new ListProfilesRequest(), cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>Creates a profile (the <c>id</c> field is ignored). Returns the assigned id.</summary>
    public Task<RpcOutcome<CreateProfileReply>> CreateProfileAsync(
        Profile profile,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _rules.CreateProfileAsync(
            new CreateProfileRequest { Profile = profile },
            cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>Updates an existing profile in place (matched by its <c>id</c>).</summary>
    public Task<RpcOutcome<UpdateProfileReply>> UpdateProfileAsync(
        Profile profile,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _rules.UpdateProfileAsync(
            new UpdateProfileRequest { Profile = profile },
            cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>Deletes a profile by id (its member rules are left intact).</summary>
    public Task<RpcOutcome<DeleteProfileReply>> DeleteProfileAsync(
        long id,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _rules.DeleteProfileAsync(
            new DeleteProfileRequest { Id = id }, cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>
    /// Activates or deactivates a profile. Activating enables its member rules (and
    /// applies its power mode); deactivating disables them. The reply's
    /// <c>ok</c>/<c>message</c> carry the server-side result.
    /// </summary>
    public Task<RpcOutcome<SetProfileActiveReply>> SetProfileActiveAsync(
        long id,
        bool active,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _rules.SetProfileActiveAsync(
            new SetProfileActiveRequest { Id = id, Active = active },
            cancellationToken: cancellationToken).ResponseAsync);

    // ----------------------------------------------------------------------
    // R3: dynamic responsiveness protection (AtlasRules, PRD §9.7.3). A watchdog
    // that, when explicitly enabled, TEMPORARILY dampens a background process
    // monopolising the CPU and auto-restores it — never touching the foreground
    // or protected-critical apps, off by default. The UI owns the config surface
    // (this pair); the dampening interventions themselves surface through the
    // existing ListInterventions with rule_id = 0 (proto R3 header). Same
    // Unimplemented→Unsupported guard so the config card degrades to a calm
    // "unavailable" state against an older service that serves these two RPCs as
    // Unimplemented (the server side lands after the UI — task brief).
    // ----------------------------------------------------------------------

    /// <summary>
    /// The current dynamic-protection config (enabled flag + CPU threshold,
    /// sustain, and auto-restore-cap durations). Against an older service this
    /// returns <see cref="RpcOutcome{T}.Unsupported"/> so the card can show a calm
    /// "unavailable" state rather than crash.
    /// </summary>
    public Task<RpcOutcome<GetDynamicProtectionReply>> GetDynamicProtectionAsync(
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _rules.GetDynamicProtectionAsync(
            new GetDynamicProtectionRequest(), cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>
    /// Saves the dynamic-protection config. Turning it on IS the consent gesture
    /// for this watchdog; turning it off (or lowering the cap) is always safe and
    /// reversible — the engine auto-restores any process it is currently easing
    /// back. The reply's <c>ok</c>/<c>message</c> carry the server-side result.
    /// </summary>
    public Task<RpcOutcome<SetDynamicProtectionReply>> SetDynamicProtectionAsync(
        DynamicProtectionConfig config,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _rules.SetDynamicProtectionAsync(
            new SetDynamicProtectionRequest { Config = config },
            cancellationToken: cancellationToken).ResponseAsync);

    // ----------------------------------------------------------------------
    // R3: signed plugin registry management (AtlasPlugins, PRD §18.3). A NEW
    // service — its own client alongside AtlasQuery/AtlasControl/AtlasRules.
    // These five RPCs are the first-party UI's registry surface: list, register,
    // enable/disable, edit the granted capabilities, and remove. Plugins are
    // out-of-process, Authenticode-signed, capability-scoped READ-ONLY extensions
    // that are OFF by default; registering an unsigned executable is refused
    // unless the user explicitly opts in (proto R3 header). Same Unimplemented→
    // Unsupported guard so the Plugins page degrades to a calm "unavailable" state
    // against an older service that serves these RPCs as Unimplemented (the server
    // side lands after the UI — task brief).
    //
    // OpenPluginSession is deliberately NOT wrapped here: that call belongs to a
    // launched plugin process exchanging its one-time nonce for a scoped session
    // token, never to the first-party UI (which does registry management only).
    // ----------------------------------------------------------------------

    /// <summary>All registered plugins (enabled and disabled), in server order.</summary>
    public Task<RpcOutcome<ListPluginsReply>> ListPluginsAsync(
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _plugins.ListPluginsAsync(
            new ListPluginsRequest(), cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>
    /// Registers the executable at <paramref name="exePath"/>, granting it the
    /// given read-only <paramref name="capabilities"/> to start with (the user
    /// edits the grant later). The service verifies the Authenticode signature
    /// first; an unsigned executable is <b>refused</b> unless
    /// <paramref name="allowUnsigned"/> is set — an explicit, unsafe opt-in. The
    /// reply's <c>ok</c>/<c>message</c> carry the result (including a plain refusal
    /// reason such as "refused: executable is not signed"); a newly registered
    /// plugin is disabled until the user enables it, so registering changes nothing
    /// on the system by itself.
    /// </summary>
    public Task<RpcOutcome<RegisterPluginReply>> RegisterPluginAsync(
        string exePath,
        IEnumerable<PluginCapability>? capabilities = null,
        bool allowUnsigned = false,
        CancellationToken cancellationToken = default)
    {
        var req = new RegisterPluginRequest
        {
            ExePath = exePath ?? string.Empty,
            AllowUnsigned = allowUnsigned,
        };
        if (capabilities is not null)
        {
            req.Requested.AddRange(capabilities);
        }
        return GuardAsync(() => _plugins.RegisterPluginAsync(
            req, cancellationToken: cancellationToken).ResponseAsync);
    }

    /// <summary>
    /// Enables or disables a plugin by id. Enabling IS the consent gesture that lets
    /// the plugin be launched with its granted read-only capabilities; disabling
    /// stops it. Off by default (proto R3 header). The reply's <c>ok</c>/<c>message</c>
    /// carry the server-side result.
    /// </summary>
    public Task<RpcOutcome<SetPluginEnabledReply>> SetPluginEnabledAsync(
        long id,
        bool enabled,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _plugins.SetPluginEnabledAsync(
            new SetPluginEnabledRequest { Id = id, Enabled = enabled },
            cancellationToken: cancellationToken).ResponseAsync);

    /// <summary>
    /// Replaces the set of read-only capabilities granted to a plugin (a re-grant).
    /// A plugin only ever gets the capabilities the user grants here — each is a
    /// read-only slice of the query surface. The reply's <c>ok</c> carries the result.
    /// </summary>
    public Task<RpcOutcome<GrantPluginCapabilitiesReply>> GrantPluginCapabilitiesAsync(
        long id,
        IEnumerable<PluginCapability> granted,
        CancellationToken cancellationToken = default)
    {
        var req = new GrantPluginCapabilitiesRequest { Id = id };
        if (granted is not null)
        {
            req.Granted.AddRange(granted);
        }
        return GuardAsync(() => _plugins.GrantPluginCapabilitiesAsync(
            req, cancellationToken: cancellationToken).ResponseAsync);
    }

    /// <summary>Removes a plugin from the registry by id. The reply's <c>ok</c> carries the result.</summary>
    public Task<RpcOutcome<RemovePluginReply>> RemovePluginAsync(
        long id,
        CancellationToken cancellationToken = default) =>
        GuardAsync(() => _plugins.RemovePluginAsync(
            new RemovePluginRequest { Id = id },
            cancellationToken: cancellationToken).ResponseAsync);

    public void Dispose() => _channel.Dispose();
}
