using Atlas.V0;
using Microsoft.UI.Xaml.Controls;

namespace Atlas.App.Views;

/// <summary>
/// The create/edit dialog for a privacy-alert rule (R2, PRD §9.10.3). Collects the
/// name, the capability to watch (All / Camera / Microphone / Location), the
/// condition (any use, background use, while locked, unknown app, or active longer
/// than a threshold), the threshold seconds when the condition needs one, and
/// whether to enable it now. Saving validates first (a rule needs a name, and a
/// positive threshold when "Active longer than" is chosen) and cancels the close on
/// failure, surfacing the reason inline. The caller reads <see cref="ResultRule"/>
/// after a Primary result and performs the create/update.
///
/// <para>
/// The copy is deliberately calm: an alert only tells the user, it never blocks an
/// app, and nothing here implies wrongdoing (proto R2 header — factual only).
/// </para>
/// </summary>
public sealed partial class PrivacyAlertEditDialog : ContentDialog
{
    private readonly long _existingId;
    private readonly long _createdMs;

    /// <param name="existing">The rule to edit, or null to create a new one.</param>
    public PrivacyAlertEditDialog(PrivacyAlertRule? existing)
    {
        InitializeComponent();

        Title = existing is null ? "New privacy alert" : "Edit privacy alert";
        _existingId = existing?.Id ?? 0;
        _createdMs = existing?.CreatedMs ?? 0;

        if (existing is not null)
        {
            LoadFrom(existing);
        }

        UpdateThresholdVisibility();
        PrimaryButtonClick += OnPrimaryClick;
    }

    /// <summary>The rule built from the form; valid only after a Primary result.</summary>
    public PrivacyAlertRule? ResultRule { get; private set; }

    private void LoadFrom(PrivacyAlertRule rule)
    {
        NameBox.Text = rule.Name;
        CapabilityBox.SelectedIndex = rule.Capability switch
        {
            CapabilityKind.Camera => 1,
            CapabilityKind.Microphone => 2,
            CapabilityKind.Location => 3,
            _ => 0,
        };
        ConditionBox.SelectedIndex = rule.Condition switch
        {
            PrivacyAlertCondition.AlertAnyUse => 0,
            PrivacyAlertCondition.AlertBackgroundUse => 1,
            PrivacyAlertCondition.AlertWhileLocked => 2,
            PrivacyAlertCondition.AlertUnknownApp => 3,
            PrivacyAlertCondition.AlertLongerThan => 4,
            _ => 0,
        };
        if (rule.ThresholdSeconds > 0)
        {
            ThresholdBox.Value = rule.ThresholdSeconds;
        }
        EnabledSwitch.IsOn = rule.Enabled;
    }

    private void ConditionBox_SelectionChanged(object sender, SelectionChangedEventArgs e)
        => UpdateThresholdVisibility();

    private void UpdateThresholdVisibility()
    {
        var tag = (ConditionBox.SelectedItem as ComboBoxItem)?.Tag as string;
        ThresholdPanel.Visibility = tag == "longer"
            ? Microsoft.UI.Xaml.Visibility.Visible
            : Microsoft.UI.Xaml.Visibility.Collapsed;
    }

    private CapabilityKind SelectedCapability() =>
        ((CapabilityBox.SelectedItem as ComboBoxItem)?.Tag as string) switch
        {
            "camera" => CapabilityKind.Camera,
            "mic" => CapabilityKind.Microphone,
            "location" => CapabilityKind.Location,
            _ => CapabilityKind.Unspecified,
        };

    private PrivacyAlertCondition SelectedCondition() =>
        ((ConditionBox.SelectedItem as ComboBoxItem)?.Tag as string) switch
        {
            "background" => PrivacyAlertCondition.AlertBackgroundUse,
            "locked" => PrivacyAlertCondition.AlertWhileLocked,
            "unknown" => PrivacyAlertCondition.AlertUnknownApp,
            "longer" => PrivacyAlertCondition.AlertLongerThan,
            _ => PrivacyAlertCondition.AlertAnyUse,
        };

    private void OnPrimaryClick(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        var name = NameBox.Text?.Trim() ?? string.Empty;
        if (name.Length == 0)
        {
            Fail(args, "Give the alert a name.");
            return;
        }

        var condition = SelectedCondition();
        uint threshold = 0;
        if (condition == PrivacyAlertCondition.AlertLongerThan)
        {
            double value = ThresholdBox.Value;
            if (double.IsNaN(value) || value < 1)
            {
                Fail(args, "Enter how many seconds counts as too long (at least 1).");
                return;
            }
            threshold = (uint)value;
        }

        ResultRule = new PrivacyAlertRule
        {
            Id = _existingId,
            Name = name,
            Enabled = EnabledSwitch.IsOn,
            Capability = SelectedCapability(),
            Condition = condition,
            ThresholdSeconds = threshold,
            CreatedMs = _createdMs,
        };
    }

    private void Fail(ContentDialogButtonClickEventArgs args, string message)
    {
        args.Cancel = true;
        ValidationBar.Message = message;
        ValidationBar.IsOpen = true;
    }
}
