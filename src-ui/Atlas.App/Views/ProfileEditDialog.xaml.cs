using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using Atlas.IpcClient;
using Atlas.V0;
using CommunityToolkit.Mvvm.ComponentModel;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace Atlas.App.Views;

/// <summary>
/// The create/edit dialog for a profile (R2, PRD §9.7.4): name, power mode, and a
/// checkable list of member rules. On Save it builds a <see cref="Profile"/> from
/// the form; the caller performs the create/update and reads
/// <see cref="ResultProfile"/>. A profile needs a name; member rules are optional.
/// </summary>
public sealed partial class ProfileEditDialog : ContentDialog
{
    private readonly long _existingId;
    private readonly bool _existingActive;
    private readonly ObservableCollection<SelectableRule> _rules = new();

    /// <param name="existing">The profile to edit, or null to create a new one.</param>
    /// <param name="allRules">Every rule, for the member picker.</param>
    public ProfileEditDialog(Profile? existing, IReadOnlyList<Rule> allRules)
    {
        InitializeComponent();

        Title = existing is null ? "New profile" : "Edit profile";
        _existingId = existing?.Id ?? 0;
        _existingActive = existing?.Active ?? false;

        var selected = existing?.RuleIds.ToHashSet() ?? new HashSet<long>();
        foreach (var rule in allRules)
        {
            _rules.Add(new SelectableRule(rule, selected.Contains(rule.Id)));
        }
        RulesList.ItemsSource = _rules;
        NoRulesNote.Visibility = _rules.Count == 0 ? Visibility.Visible : Visibility.Collapsed;

        if (existing is not null)
        {
            NameBox.Text = existing.Name;
            PowerModeBox.SelectedIndex = existing.PowerMode switch
            {
                "PowerSaver" => 1,
                "Balanced" => 2,
                "HighPerformance" => 3,
                _ => 0,
            };
        }

        PrimaryButtonClick += OnPrimaryClick;
    }

    /// <summary>The profile built from the form; valid only after a Primary result.</summary>
    public Profile? ResultProfile { get; private set; }

    private void OnPrimaryClick(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        var name = NameBox.Text?.Trim() ?? string.Empty;
        if (name.Length == 0)
        {
            args.Cancel = true;
            ValidationBar.Message = "Give the profile a name.";
            ValidationBar.IsOpen = true;
            return;
        }

        var powerMode = (PowerModeBox.SelectedItem as ComboBoxItem)?.Tag as string ?? string.Empty;

        var profile = new Profile
        {
            Id = _existingId,
            Name = name,
            PowerMode = powerMode,
            Active = _existingActive,
        };
        profile.RuleIds.AddRange(_rules.Where(r => r.IsSelected).Select(r => r.Id));
        ResultProfile = profile;
    }
}

/// <summary>A rule shown in the profile member picker, with a checkbox binding.</summary>
public sealed partial class SelectableRule : ObservableObject
{
    [ObservableProperty] private bool _isSelected;

    public SelectableRule(Rule rule, bool selected)
    {
        Id = rule.Id;
        Name = string.IsNullOrWhiteSpace(rule.Name) ? $"Rule {rule.Id}" : rule.Name;
        Summary = RulesFormatter.RuleSummary(rule);
        _isSelected = selected;
    }

    public long Id { get; }
    public string Name { get; }
    public string Summary { get; }
}
