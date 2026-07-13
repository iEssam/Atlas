using Atlas.V0;

namespace Atlas.IpcClient;

/// <summary>
/// An in-memory <see cref="IActionBroker"/> that mimics the broker's two-phase
/// contract without a server. Used to prove the safe-action dialog UX before the
/// backend merge (task brief §4) and to drive unit tests. Configure it to allow
/// or deny, to report unsupported, or to fault.
/// </summary>
public sealed class FakeActionBroker : IActionBroker
{
    private readonly PrepareActionReply _prepareReply;
    private readonly bool _prepareUnsupported;
    private readonly bool _executeUnsupported;
    private readonly bool _executeSuccess;
    private readonly string _executeMessage;

    /// <summary>The last token passed to <see cref="ExecuteAsync"/> (test hook).</summary>
    public string? LastExecutedToken { get; private set; }

    /// <summary>How many times <see cref="ExecuteAsync"/> was invoked.</summary>
    public int ExecuteCallCount { get; private set; }

    private FakeActionBroker(
        PrepareActionReply prepareReply,
        bool prepareUnsupported,
        bool executeUnsupported,
        bool executeSuccess,
        string executeMessage)
    {
        _prepareReply = prepareReply;
        _prepareUnsupported = prepareUnsupported;
        _executeUnsupported = executeUnsupported;
        _executeSuccess = executeSuccess;
        _executeMessage = executeMessage;
    }

    /// <summary>An allowing broker with a fixed token and the given risk.</summary>
    public static FakeActionBroker Allowing(
        ActionRisk? risk = null,
        string token = "consent-token-abc",
        bool executeSuccess = true,
        string executeMessage = "Done.")
    {
        var reply = new PrepareActionReply
        {
            Allowed = true,
            ConsentToken = token,
            TokenExpiresMs = 0,
            Risk = risk ?? new ActionRisk(),
        };
        return new FakeActionBroker(reply, false, false, executeSuccess, executeMessage);
    }

    /// <summary>A denying broker with a reason and (optional) risk picture.</summary>
    public static FakeActionBroker Denying(string reason, ActionRisk? risk = null)
    {
        var reply = new PrepareActionReply
        {
            Allowed = false,
            DenialReason = reason,
            Risk = risk ?? new ActionRisk { IsCritical = true },
        };
        return new FakeActionBroker(reply, false, false, false, string.Empty);
    }

    /// <summary>A broker whose Prepare reports Unimplemented (old server).</summary>
    public static FakeActionBroker Unsupported() =>
        new(new PrepareActionReply(), true, true, false, string.Empty);

    public Task<RpcOutcome<PrepareActionReply>> PrepareAsync(
        uint pid, long createTime100ns, ProcessActionKind action, CancellationToken ct = default) =>
        Task.FromResult(_prepareUnsupported
            ? RpcOutcome<PrepareActionReply>.Unsupported("not implemented")
            : RpcOutcome<PrepareActionReply>.Ok(_prepareReply));

    public Task<RpcOutcome<ExecuteActionReply>> ExecuteAsync(
        string consentToken, CancellationToken ct = default)
    {
        ExecuteCallCount++;
        LastExecutedToken = consentToken;
        if (_executeUnsupported)
        {
            return Task.FromResult(RpcOutcome<ExecuteActionReply>.Unsupported("not implemented"));
        }
        return Task.FromResult(RpcOutcome<ExecuteActionReply>.Ok(new ExecuteActionReply
        {
            Success = _executeSuccess,
            Message = _executeMessage,
        }));
    }
}
