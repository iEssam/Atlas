using Atlas.V0;

namespace Atlas.IpcClient;

/// <summary>The phase the <see cref="SafeActionFlow"/> is in.</summary>
public enum SafeActionPhase
{
    /// <summary>Initial: nothing prepared yet.</summary>
    Idle,

    /// <summary>PrepareAction in flight.</summary>
    Preparing,

    /// <summary>Prepared and the broker allowed it — an execute is offered.</summary>
    Allowed,

    /// <summary>Prepared and the broker denied it — no execute is offered.</summary>
    Denied,

    /// <summary>The server does not implement the action broker (too old).</summary>
    Unsupported,

    /// <summary>ExecuteAction in flight.</summary>
    Executing,

    /// <summary>Execute finished (see <see cref="SafeActionFlow.ResultMessage"/>).</summary>
    Completed,

    /// <summary>Prepare or execute threw (transport/server error).</summary>
    Faulted,
}

/// <summary>
/// The pure, UI-agnostic state machine behind the safe-action dialog (PRD
/// §9.22). It runs the two-phase prepare→execute handshake against any
/// <see cref="IActionBroker"/>, and — critically — <b>only permits Execute when
/// the broker allowed the action and returned a non-empty consent token</b>.
/// A denied prepare, an unsupported server, or a missing token all leave
/// <see cref="CanExecute"/> false, so the dialog can never fire a destructive
/// call the broker didn't sanction.
///
/// <para>
/// This lives in Atlas.IpcClient (not the WinUI app) so it is unit-testable
/// with a fake broker and no live server (task brief §4).
/// </para>
/// </summary>
public sealed class SafeActionFlow
{
    private readonly IActionBroker _broker;
    private readonly uint _pid;
    private readonly long _createTime100ns;

    private string? _consentToken;

    public SafeActionFlow(
        IActionBroker broker, uint pid, long createTime100ns, ProcessActionKind action)
    {
        _broker = broker;
        _pid = pid;
        _createTime100ns = createTime100ns;
        Action = action;
    }

    /// <summary>The action this flow prepares/executes.</summary>
    public ProcessActionKind Action { get; }

    /// <summary>Current phase.</summary>
    public SafeActionPhase Phase { get; private set; } = SafeActionPhase.Idle;

    /// <summary>The broker's risk assessment (null until a successful prepare).</summary>
    public ActionRisk? Risk { get; private set; }

    /// <summary>The denial reason when <see cref="Phase"/> is Denied.</summary>
    public string DenialReason { get; private set; } = string.Empty;

    /// <summary>The execute result / error message, once known.</summary>
    public string ResultMessage { get; private set; } = string.Empty;

    /// <summary>True when the last execute reported success.</summary>
    public bool Succeeded { get; private set; }

    /// <summary>The affirmative-button caption (e.g. "Suspend", "End").</summary>
    public string ActionVerb => HistoryFormatter.ActionVerb(Action);

    /// <summary>The reversibility sentence for the dialog subtitle.</summary>
    public string ReversibilityText => HistoryFormatter.ReversibilityText(Action);

    /// <summary>The formatted multi-line risk summary (empty when no risk yet).</summary>
    public string RiskSummary => HistoryFormatter.RiskSummary(Risk);

    /// <summary>
    /// The one gate the affirmative button binds to: only true once the broker
    /// has allowed the action and handed back a usable consent token.
    /// </summary>
    public bool CanExecute =>
        Phase == SafeActionPhase.Allowed && !string.IsNullOrEmpty(_consentToken);

    /// <summary>
    /// Runs phase 1. Populates <see cref="Risk"/> and, on allow, arms
    /// <see cref="CanExecute"/>. Never throws for an unsupported server (maps to
    /// the Unsupported phase); genuine faults land in the Faulted phase.
    /// </summary>
    public async Task PrepareAsync(CancellationToken ct = default)
    {
        Phase = SafeActionPhase.Preparing;
        try
        {
            var outcome = await _broker
                .PrepareAsync(_pid, _createTime100ns, Action, ct)
                .ConfigureAwait(false);

            if (!outcome.Supported)
            {
                Phase = SafeActionPhase.Unsupported;
                ResultMessage = "Safe actions are unavailable — the service is too old.";
                return;
            }

            var reply = outcome.Value;
            Risk = reply.Risk;

            if (reply.Allowed && !string.IsNullOrEmpty(reply.ConsentToken))
            {
                _consentToken = reply.ConsentToken;
                Phase = SafeActionPhase.Allowed;
            }
            else
            {
                // Defensive: a broker that claims allowed but omits a token is
                // treated as a denial so no execute can proceed.
                DenialReason = string.IsNullOrEmpty(reply.DenialReason)
                    ? (reply.Allowed ? "No consent token was issued." : "Action not permitted.")
                    : reply.DenialReason;
                Phase = SafeActionPhase.Denied;
            }
        }
        catch (Exception ex)
        {
            Phase = SafeActionPhase.Faulted;
            ResultMessage = ex.Message;
        }
    }

    /// <summary>
    /// Runs phase 2 with the stored consent token. No-ops unless
    /// <see cref="CanExecute"/> is true, so a denied/unprepared flow can never
    /// execute. The token is single-use and is cleared afterwards.
    /// </summary>
    public async Task ExecuteAsync(CancellationToken ct = default)
    {
        if (!CanExecute)
        {
            return;
        }

        var token = _consentToken!;
        _consentToken = null; // single-use: prevent a second execute
        Phase = SafeActionPhase.Executing;
        try
        {
            var outcome = await _broker.ExecuteAsync(token, ct).ConfigureAwait(false);
            if (!outcome.Supported)
            {
                Phase = SafeActionPhase.Unsupported;
                ResultMessage = "Safe actions are unavailable — the service is too old.";
                return;
            }

            Succeeded = outcome.Value.Success;
            ResultMessage = outcome.Value.Message;
            Phase = SafeActionPhase.Completed;
        }
        catch (Exception ex)
        {
            Phase = SafeActionPhase.Faulted;
            ResultMessage = ex.Message;
        }
    }
}
