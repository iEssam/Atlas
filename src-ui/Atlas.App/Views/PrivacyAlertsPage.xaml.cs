using System;
using Atlas.App.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;

namespace Atlas.App.Views;

/// <summary>
/// The Privacy Alerts page (R2, PRD §9.10.3): the list of alert rules over camera /
/// microphone / location usage with an enable/disable toggle and per-row Edit /
/// Delete, plus a "Recent alerts" log of rules that have fired. Create/edit opens
/// <see cref="PrivacyAlertEditDialog"/>; Delete asks for confirmation first.
///
/// <para>
/// The framing stays calm and factual throughout — a fired alert means "you asked
/// to be told about this", never "a threat". The whole page degrades gracefully
/// when the privacy-alert RPCs are unavailable (they land after this UI). Set
/// <c>ATLAS_FAKE_PRIVACY_ALERTS=1</c> to explore the UX with demo data.
/// </para>
/// </summary>
public sealed partial class PrivacyAlertsPage : Page
{
    private readonly string? _who;
    private readonly bool _fake;

    public PrivacyAlertsViewModel ViewModel { get; }

    public PrivacyAlertsPage()
    {
        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        _who = string.IsNullOrEmpty(who) ? null : who;
        _fake = Environment.GetEnvironmentVariable("ATLAS_FAKE_PRIVACY_ALERTS") == "1";

        ViewModel = new PrivacyAlertsViewModel(DispatcherQueue, _who, _fake);
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

    private async void NewAlert_Click(object sender, RoutedEventArgs e)
    {
        var dialog = new PrivacyAlertEditDialog(existing: null) { XamlRoot = XamlRoot };
        var result = await dialog.ShowAsync();
        if (result == ContentDialogResult.Primary && dialog.ResultRule is not null)
        {
            await CreateAsync(dialog.ResultRule);
        }
    }

    private async void EditAlert_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not PrivacyAlertRuleRowViewModel row)
        {
            return;
        }
        var dialog = new PrivacyAlertEditDialog(row.Rule) { XamlRoot = XamlRoot };
        var result = await dialog.ShowAsync();
        if (result == ContentDialogResult.Primary && dialog.ResultRule is not null)
        {
            await UpdateAsync(dialog.ResultRule);
        }
    }

    private async void DeleteAlert_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not PrivacyAlertRuleRowViewModel row)
        {
            return;
        }

        var confirm = new ContentDialog
        {
            XamlRoot = XamlRoot,
            Title = "Delete alert?",
            Content = $"“{row.Name}” will be removed. Atlas will stop watching for this — your earlier alerts are kept.",
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
            await ShowErrorAsync("Couldn't delete the alert", message);
            return;
        }
        await ViewModel.RefreshAsync();
    }

    private async void EnabledToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (sender is not ToggleSwitch toggle ||
            toggle.DataContext is not PrivacyAlertRuleRowViewModel row)
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
            await ShowErrorAsync("Couldn't change the alert", message);
        }
    }

    private async System.Threading.Tasks.Task CreateAsync(Atlas.V0.PrivacyAlertRule rule)
    {
        if (_fake)
        {
            await ViewModel.RefreshAsync();
            return;
        }
        try
        {
            using var channel = Atlas.IpcClient.AtlasChannel.Connect(_who);
            var outcome = await channel.CreatePrivacyAlertRuleAsync(rule);
            if (!outcome.Supported)
            {
                await ShowErrorAsync("Couldn't create the alert", "This service is too old to manage privacy alerts.");
                return;
            }
        }
        catch (Exception ex)
        {
            await ShowErrorAsync("Couldn't create the alert", ex.Message);
            return;
        }
        await ViewModel.RefreshAsync();
    }

    private async System.Threading.Tasks.Task UpdateAsync(Atlas.V0.PrivacyAlertRule rule)
    {
        if (_fake)
        {
            await ViewModel.RefreshAsync();
            return;
        }
        try
        {
            using var channel = Atlas.IpcClient.AtlasChannel.Connect(_who);
            var outcome = await channel.UpdatePrivacyAlertRuleAsync(rule);
            if (!outcome.Supported)
            {
                await ShowErrorAsync("Couldn't save the alert", "This service is too old to manage privacy alerts.");
                return;
            }
            if (!outcome.Value.Ok)
            {
                await ShowErrorAsync("Couldn't save the alert", "The service rejected the change.");
                return;
            }
        }
        catch (Exception ex)
        {
            await ShowErrorAsync("Couldn't save the alert", ex.Message);
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
