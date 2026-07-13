using Atlas.V0;

namespace Atlas.IpcClient;

/// <summary>
/// The two-phase safe-action broker as the UI sees it (PRD §9.22). Abstracted
/// so the dialog flow can be driven by a live <see cref="AtlasChannel"/> in the
/// app and by an in-memory fake in unit tests and the design-time path — the
/// live service won't implement PrepareAction/ExecuteAction until the backend
/// merge, so the UX is proven without it (task brief §4).
/// </summary>
public interface IActionBroker
{
    /// <summary>Phase 1: assess the action, returning the risk picture + token.</summary>
    Task<RpcOutcome<PrepareActionReply>> PrepareAsync(
        uint pid, long createTime100ns, ProcessActionKind action, CancellationToken ct = default);

    /// <summary>Phase 2: perform exactly the prepared action via its token.</summary>
    Task<RpcOutcome<ExecuteActionReply>> ExecuteAsync(
        string consentToken, CancellationToken ct = default);
}

/// <summary>Adapts a live <see cref="AtlasChannel"/> to <see cref="IActionBroker"/>.</summary>
public sealed class ChannelActionBroker : IActionBroker
{
    private readonly AtlasChannel _channel;

    public ChannelActionBroker(AtlasChannel channel) => _channel = channel;

    public Task<RpcOutcome<PrepareActionReply>> PrepareAsync(
        uint pid, long createTime100ns, ProcessActionKind action, CancellationToken ct = default) =>
        _channel.PrepareActionAsync(pid, createTime100ns, action, ct);

    public Task<RpcOutcome<ExecuteActionReply>> ExecuteAsync(
        string consentToken, CancellationToken ct = default) =>
        _channel.ExecuteActionAsync(consentToken, ct);
}
