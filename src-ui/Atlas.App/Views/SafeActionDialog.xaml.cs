using System;
using System.Threading.Tasks;
using Atlas.IpcClient;
using Atlas.V0;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace Atlas.App.Views;

/// <summary>
/// The safe-action confirmation dialog (M6, PRD §9.22). It runs the two-phase
/// <see cref="SafeActionFlow"/> against an <see cref="IActionBroker"/>:
/// <list type="number">
/// <item>On open, <b>Prepare</b> is called and the risk picture is shown —
///   critical/system flags, visible-window and child counts, the broker's
///   notes, and the action's reversibility.</item>
/// <item>If <b>denied</b>, the denial reason is shown and no affirmative button
///   is offered.</item>
/// <item>If <b>allowed</b>, the single affirmative button (labelled per the
///   action: "Suspend"/"End"/…) calls <b>Execute</b> with the consent token.</item>
/// </list>
///
/// <para>
/// The affirmative button is <b>never the default focus</b>: <c>DefaultButton</c>
/// is Close (Cancel), and the primary button is only added once the action is
/// allowed. The dialog can be driven by a live channel broker or, before the
/// backend lands, by a <see cref="FakeActionBroker"/> — same UX either way
/// (task brief §4).
/// </para>
/// </summary>
public sealed partial class SafeActionDialog : ContentDialog
{
    private readonly SafeActionFlow _flow;
    private readonly string _targetLabel;

    /// <param name="broker">Live or fake action broker.</param>
    /// <param name="pid">Target process id.</param>
    /// <param name="createTime100ns">Process create-time (PID-reuse guard).</param>
    /// <param name="action">The action to prepare/execute.</param>
    /// <param name="targetLabel">Human label, e.g. "chrome.exe (pid 4242)".</param>
    public SafeActionDialog(
        IActionBroker broker,
        uint pid,
        long createTime100ns,
        ProcessActionKind action,
        string targetLabel)
    {
        _flow = new SafeActionFlow(broker, pid, createTime100ns, action);
        _targetLabel = targetLabel;

        InitializeComponent();

        Title = $"{HistoryFormatter.ActionVerb(action)} process";
        TargetText.Text = $"{HistoryFormatter.ActionVerb(action)} {targetLabel}?";
        ReversibilityText.Text = _flow.ReversibilityText;

        // Wire the two-phase handshake to the dialog lifecycle.
        Opened += OnOpened;
        PrimaryButtonClick += OnPrimaryClick;
    }

    /// <summary>True after a successful execute (for the caller to react).</summary>
    public bool ActionSucceeded => _flow.Succeeded && _flow.Phase == SafeActionPhase.Completed;

    private async void OnOpened(ContentDialog sender, ContentDialogOpenedEventArgs args)
    {
        // Phase 1: prepare and render the risk picture.
        SetBusy(true);
        await _flow.PrepareAsync();
        SetBusy(false);

        RiskText.Text = string.IsNullOrEmpty(_flow.RiskSummary)
            ? "No elevated risks were flagged for this action."
            : _flow.RiskSummary;

        switch (_flow.Phase)
        {
            case SafeActionPhase.Allowed:
                // Offer the single affirmative button. It is NOT the default
                // button — Close/Cancel remains the default focus.
                PrimaryButtonText = _flow.ActionVerb;
                IsPrimaryButtonEnabled = true;
                DefaultButton = ContentDialogButton.Close;
                break;

            case SafeActionPhase.Denied:
                DenialBar.Message = _flow.DenialReason;
                DenialBar.IsOpen = true;
                // No execute offered.
                PrimaryButtonText = string.Empty;
                break;

            case SafeActionPhase.Unsupported:
                ResultBar.Severity = InfoBarSeverity.Warning;
                ResultBar.Title = "Unavailable";
                ResultBar.Message = _flow.ResultMessage;
                ResultBar.IsOpen = true;
                PrimaryButtonText = string.Empty;
                break;

            case SafeActionPhase.Faulted:
                ResultBar.Severity = InfoBarSeverity.Error;
                ResultBar.Title = "Could not assess the action";
                ResultBar.Message = _flow.ResultMessage;
                ResultBar.IsOpen = true;
                PrimaryButtonText = string.Empty;
                break;
        }
    }

    private async void OnPrimaryClick(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        // Keep the dialog open while executing; only close on the second pass.
        var deferral = args.GetDeferral();
        try
        {
            if (!_flow.CanExecute)
            {
                args.Cancel = true; // defensive: never execute without consent
                return;
            }

            SetBusy(true);
            IsPrimaryButtonEnabled = false;
            await _flow.ExecuteAsync();
            SetBusy(false);

            if (_flow.Phase == SafeActionPhase.Completed && _flow.Succeeded)
            {
                // Success: let the dialog close.
                return;
            }

            // Failure / unsupported / fault: surface it and keep the dialog open.
            args.Cancel = true;
            ResultBar.Severity = _flow.Succeeded ? InfoBarSeverity.Success : InfoBarSeverity.Error;
            ResultBar.Title = _flow.Phase == SafeActionPhase.Unsupported ? "Unavailable" : "Action failed";
            ResultBar.Message = string.IsNullOrEmpty(_flow.ResultMessage)
                ? "The action did not complete."
                : _flow.ResultMessage;
            ResultBar.IsOpen = true;
            PrimaryButtonText = string.Empty; // token is single-use; no retry
        }
        finally
        {
            deferral.Complete();
        }
    }

    private void SetBusy(bool busy)
    {
        BusyRing.IsActive = busy;
    }
}
