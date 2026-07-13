using System;
using Atlas.App.ViewModels;
using Atlas.IpcClient;
using Atlas.V0;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml.Controls;

namespace Atlas.App.Views;

/// <summary>
/// The create/edit dialog for a performance rule (R2, PRD §9.7). Collects the
/// match image, trigger, priority, affinity (with a custom core list when
/// "Custom"), efficiency mode, precedence, and whether to enable it now. Its
/// centerpiece is the inline <b>Preview impact</b> section (PRD §9.7.5): a
/// <see cref="SimulationResultView"/> that runs the pure dry-run
/// <c>SimulateRule</c> against the rule as currently edited, so the user sees
/// exactly which running processes would change — including protected-critical
/// ones Atlas won't touch — before committing. Nothing is applied until Save.
///
/// <para>
/// The primary "Save" button validates first (a rule needs a name and a match
/// image, and a valid custom mask when chosen) and cancels the close on failure,
/// surfacing the reason inline. The caller reads <see cref="ResultRule"/> after a
/// Primary result and performs the create/update.
/// </para>
/// </summary>
public sealed partial class RuleEditDialog : ContentDialog
{
    private readonly long _existingId;
    private readonly long _createdMs;
    private readonly SimulationResultViewModel _sim;

    /// <param name="existing">The rule to edit, or null to create a new one.</param>
    /// <param name="who">Pipe discriminator for the live channel.</param>
    /// <param name="fake">True to preview against demo data (no backend).</param>
    public RuleEditDialog(Rule? existing, string? who, bool fake)
    {
        InitializeComponent();

        _sim = new SimulationResultViewModel(DispatcherQueue, who, fake);
        SimView.ViewModel = _sim;

        Title = existing is null ? "New rule" : "Edit rule";
        _existingId = existing?.Id ?? 0;
        _createdMs = existing?.CreatedMs ?? 0;

        if (existing is not null)
        {
            LoadFrom(existing);
        }

        PrimaryButtonClick += OnPrimaryClick;
    }

    /// <summary>The rule built from the form; valid only after a Primary result.</summary>
    public Rule? ResultRule { get; private set; }

    private void LoadFrom(Rule rule)
    {
        NameBox.Text = rule.Name;
        MatchBox.Text = rule.MatchImage;
        TriggerBox.SelectedIndex = rule.Trigger switch
        {
            RuleTrigger.WhileRunning => 0,
            RuleTrigger.OnAcPower => 1,
            RuleTrigger.OnDcPower => 2,
            RuleTrigger.OnFullscreen => 3,
            _ => 0,
        };

        var action = rule.Action ?? new RuleAction();
        PriorityBox.SelectedIndex = action.Priority switch
        {
            PriorityClass.PriorityIdle => 1,
            PriorityClass.PriorityBelowNormal => 2,
            PriorityClass.PriorityNormal => 3,
            PriorityClass.PriorityAboveNormal => 4,
            PriorityClass.PriorityHigh => 5,
            _ => 0,
        };
        AffinityBox.SelectedIndex = action.AffinityMode switch
        {
            CoreAffinityMode.AllCores => 1,
            CoreAffinityMode.PreferPCores => 2,
            CoreAffinityMode.PreferECores => 3,
            CoreAffinityMode.CustomMask => 4,
            _ => 0,
        };
        if (action.AffinityMode == CoreAffinityMode.CustomMask)
        {
            CustomMaskBox.Text = RulesFormatter.AffinityMaskText(action.AffinityMask)
                .Replace("cores ", string.Empty, StringComparison.Ordinal);
        }
        EcoSwitch.IsOn = action.EcoQos;
        PrecedenceBox.Value = rule.Precedence;
        EnabledSwitch.IsOn = rule.Enabled;

        UpdateCustomMaskVisibility();
    }

    private void AffinityBox_SelectionChanged(object sender, SelectionChangedEventArgs e)
        => UpdateCustomMaskVisibility();

    private void UpdateCustomMaskVisibility()
    {
        var tag = (AffinityBox.SelectedItem as ComboBoxItem)?.Tag as string;
        CustomMaskPanel.Visibility = tag == "custom"
            ? Microsoft.UI.Xaml.Visibility.Visible
            : Microsoft.UI.Xaml.Visibility.Collapsed;
    }

    private RuleTrigger SelectedTrigger() => ((TriggerBox.SelectedItem as ComboBoxItem)?.Tag as string) switch
    {
        "ac" => RuleTrigger.OnAcPower,
        "dc" => RuleTrigger.OnDcPower,
        "fullscreen" => RuleTrigger.OnFullscreen,
        _ => RuleTrigger.WhileRunning,
    };

    private PriorityClass SelectedPriority() => ((PriorityBox.SelectedItem as ComboBoxItem)?.Tag as string) switch
    {
        "idle" => PriorityClass.PriorityIdle,
        "below" => PriorityClass.PriorityBelowNormal,
        "normal" => PriorityClass.PriorityNormal,
        "above" => PriorityClass.PriorityAboveNormal,
        "high" => PriorityClass.PriorityHigh,
        _ => PriorityClass.Unspecified,
    };

    private CoreAffinityMode SelectedAffinityMode() => ((AffinityBox.SelectedItem as ComboBoxItem)?.Tag as string) switch
    {
        "all" => CoreAffinityMode.AllCores,
        "p" => CoreAffinityMode.PreferPCores,
        "e" => CoreAffinityMode.PreferECores,
        "custom" => CoreAffinityMode.CustomMask,
        _ => CoreAffinityMode.CoreAffinityUnspecified,
    };

    /// <summary>
    /// Builds a rule from the current form. <paramref name="error"/> is set (and
    /// the result is null) when a custom mask is chosen but doesn't parse.
    /// </summary>
    private Rule? TryBuildRule(out string? error)
    {
        error = null;
        var mode = SelectedAffinityMode();
        ulong mask = 0;
        if (mode == CoreAffinityMode.CustomMask)
        {
            if (!RulesFormatter.TryParseCoreList(CustomMaskBox.Text, out mask) || mask == 0)
            {
                error = "Enter at least one core, e.g. 0-3,8.";
                return null;
            }
        }

        return new Rule
        {
            Id = _existingId,
            Name = NameBox.Text?.Trim() ?? string.Empty,
            Enabled = EnabledSwitch.IsOn,
            MatchImage = MatchBox.Text?.Trim() ?? string.Empty,
            Trigger = SelectedTrigger(),
            Precedence = (int)PrecedenceBox.Value,
            CreatedMs = _createdMs,
            Action = new RuleAction
            {
                Priority = SelectedPriority(),
                AffinityMode = mode,
                AffinityMask = mask,
                EcoQos = EcoSwitch.IsOn,
            },
        };
    }

    private void OnPrimaryClick(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        var name = NameBox.Text?.Trim() ?? string.Empty;
        var match = MatchBox.Text?.Trim() ?? string.Empty;

        if (name.Length == 0)
        {
            Fail(args, "Give the rule a name.");
            return;
        }
        if (match.Length == 0)
        {
            Fail(args, "Enter the process image this rule matches, e.g. chrome.exe.");
            return;
        }

        var rule = TryBuildRule(out var error);
        if (rule is null)
        {
            Fail(args, error ?? "Check the rule and try again.");
            return;
        }

        ResultRule = rule;
    }

    private void Fail(ContentDialogButtonClickEventArgs args, string message)
    {
        args.Cancel = true;
        ValidationBar.Message = message;
        ValidationBar.IsOpen = true;
    }

    private async void PreviewButton_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        var rule = TryBuildRule(out var error);
        if (rule is null)
        {
            ValidationBar.Message = error ?? "Check the rule before previewing.";
            ValidationBar.IsOpen = true;
            return;
        }
        ValidationBar.IsOpen = false;
        await _sim.RunAsync(rule);
    }
}
