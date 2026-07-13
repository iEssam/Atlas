using System;
using Atlas.App.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;

namespace Atlas.App.Views;

/// <summary>
/// The Rules &amp; Optimization page (R2, PRD §9.7): the list of performance rules
/// with an enable/disable toggle and per-row Preview / Edit / Delete, plus the
/// "Active interventions" transparency card showing what Atlas is currently doing
/// to the system. Create/edit opens <see cref="RuleEditDialog"/> (with the inline
/// simulation preview); Preview opens the standalone
/// <see cref="SimulationPreviewDialog"/>; Delete asks for confirmation first.
///
/// <para>
/// The whole page degrades gracefully when the AtlasRules service is unavailable
/// (it lands after this UI). Set <c>ATLAS_FAKE_RULES=1</c> to explore the UX with
/// demo data.
/// </para>
/// </summary>
public sealed partial class RulesPage : Page
{
    private readonly string? _who;
    private readonly bool _fake;

    public RulesViewModel ViewModel { get; }

    public RulesPage()
    {
        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        _who = string.IsNullOrEmpty(who) ? null : who;
        _fake = Environment.GetEnvironmentVariable("ATLAS_FAKE_RULES") == "1";

        ViewModel = new RulesViewModel(DispatcherQueue, _who, _fake);
        InitializeComponent();

        if (_fake)
        {
            DemoBadge.Visibility = Visibility.Visible;
        }
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        _ = ViewModel.RefreshAsync();
    }

    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        base.OnNavigatedFrom(e);
        ViewModel.Stop();
    }

    private async void NewRule_Click(object sender, RoutedEventArgs e)
    {
        var dialog = new RuleEditDialog(existing: null, _who, _fake) { XamlRoot = XamlRoot };
        var result = await dialog.ShowAsync();
        if (result == ContentDialogResult.Primary && dialog.ResultRule is not null)
        {
            await CreateAsync(dialog.ResultRule);
        }
    }

    private async void EditRule_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not RuleRowViewModel row)
        {
            return;
        }
        var dialog = new RuleEditDialog(row.Rule, _who, _fake) { XamlRoot = XamlRoot };
        var result = await dialog.ShowAsync();
        if (result == ContentDialogResult.Primary && dialog.ResultRule is not null)
        {
            await UpdateAsync(dialog.ResultRule);
        }
    }

    private async void PreviewRule_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not RuleRowViewModel row)
        {
            return;
        }
        var dialog = new SimulationPreviewDialog(row.Rule, _who, _fake) { XamlRoot = XamlRoot };
        await dialog.ShowAsync();
    }

    private async void DeleteRule_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not RuleRowViewModel row)
        {
            return;
        }

        var confirm = new ContentDialog
        {
            XamlRoot = XamlRoot,
            Title = "Delete rule?",
            Content = $"“{row.Name}” will be removed. Any changes it's currently applying will be reverted — the affected processes return to their previous priority, cores, and efficiency mode.",
            PrimaryButtonText = "Delete",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
        };
        if (await confirm.ShowAsync() != ContentDialogResult.Primary)
        {
            return;
        }

        var (ok, message) = await ViewModel.DeleteRuleAsync(row.Id);
        if (!ok)
        {
            await ShowErrorAsync("Couldn't delete the rule", message);
            return;
        }
        await ViewModel.RefreshAsync();
    }

    private async void EnabledToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (sender is not ToggleSwitch toggle ||
            toggle.DataContext is not RuleRowViewModel row)
        {
            return;
        }

        // Skip the toggle event raised while the template applies the bound
        // initial value — only act on a genuine user-driven change.
        if (row.Enabled == toggle.IsOn)
        {
            return;
        }

        bool desired = toggle.IsOn;
        var (ok, message) = await ViewModel.SetEnabledAsync(row.Id, desired);
        if (!ok)
        {
            // Revert the switch to the last known good state and explain.
            toggle.IsOn = row.Enabled;
            await ShowErrorAsync("Couldn't change the rule", message);
        }
    }

    private async System.Threading.Tasks.Task CreateAsync(Atlas.V0.Rule rule)
    {
        if (_fake)
        {
            await ViewModel.RefreshAsync();
            return;
        }
        try
        {
            using var channel = Atlas.IpcClient.AtlasChannel.Connect(_who);
            var outcome = await channel.CreateRuleAsync(rule);
            if (!outcome.Supported)
            {
                await ShowErrorAsync("Couldn't create the rule", "This service is too old to manage rules.");
                return;
            }
        }
        catch (Exception ex)
        {
            await ShowErrorAsync("Couldn't create the rule", ex.Message);
            return;
        }
        await ViewModel.RefreshAsync();
    }

    private async System.Threading.Tasks.Task UpdateAsync(Atlas.V0.Rule rule)
    {
        if (_fake)
        {
            await ViewModel.RefreshAsync();
            return;
        }
        try
        {
            using var channel = Atlas.IpcClient.AtlasChannel.Connect(_who);
            var outcome = await channel.UpdateRuleAsync(rule);
            if (!outcome.Supported)
            {
                await ShowErrorAsync("Couldn't save the rule", "This service is too old to manage rules.");
                return;
            }
            if (!outcome.Value.Ok)
            {
                await ShowErrorAsync("Couldn't save the rule",
                    string.IsNullOrEmpty(outcome.Value.Message) ? "The service rejected the change." : outcome.Value.Message);
                return;
            }
        }
        catch (Exception ex)
        {
            await ShowErrorAsync("Couldn't save the rule", ex.Message);
            return;
        }
        await ViewModel.RefreshAsync();
    }

    private async System.Threading.Tasks.Task ShowErrorAsync(string title, string message)
    {
        var dialog = new ContentDialog
        {
            XamlRoot = XamlRoot,
            Title = title,
            Content = string.IsNullOrEmpty(message) ? "Something went wrong." : message,
            CloseButtonText = "OK",
            DefaultButton = ContentDialogButton.Close,
        };
        await dialog.ShowAsync();
    }
}
