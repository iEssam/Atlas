using System;
using System.Threading.Tasks;
using Atlas.App.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;

namespace Atlas.App.Views;

/// <summary>
/// The Profiles page (R2, PRD §9.7.4): a card list of profiles (name, power mode,
/// member rules, active toggle) with create/edit and delete. Activating a profile
/// confirms first, listing exactly which rules it will turn on — the user always
/// sees what changes. Degrades gracefully when AtlasRules is unavailable; set
/// <c>ATLAS_FAKE_RULES=1</c> for demo data.
/// </summary>
public sealed partial class ProfilesPage : Page
{
    private readonly string? _who;
    private readonly bool _fake;

    public ProfilesViewModel ViewModel { get; }

    public ProfilesPage()
    {
        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        _who = string.IsNullOrEmpty(who) ? null : who;
        _fake = Environment.GetEnvironmentVariable("ATLAS_FAKE_RULES") == "1";

        ViewModel = new ProfilesViewModel(DispatcherQueue, _who, _fake);
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

    private async void NewProfile_Click(object sender, RoutedEventArgs e)
    {
        var dialog = new ProfileEditDialog(existing: null, ViewModel.AllRules) { XamlRoot = XamlRoot };
        var result = await dialog.ShowAsync();
        if (result == ContentDialogResult.Primary && dialog.ResultProfile is not null)
        {
            await CreateAsync(dialog.ResultProfile);
        }
    }

    private async void EditProfile_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not ProfileRowViewModel row)
        {
            return;
        }
        var dialog = new ProfileEditDialog(row.Profile, ViewModel.AllRules) { XamlRoot = XamlRoot };
        var result = await dialog.ShowAsync();
        if (result == ContentDialogResult.Primary && dialog.ResultProfile is not null)
        {
            await UpdateAsync(dialog.ResultProfile);
        }
    }

    private async void DeleteProfile_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not ProfileRowViewModel row)
        {
            return;
        }

        var confirm = new ContentDialog
        {
            XamlRoot = XamlRoot,
            Title = "Delete profile?",
            Content = $"“{row.Name}” will be removed. Its member rules are kept — only the profile bundle is deleted.",
            PrimaryButtonText = "Delete",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
        };
        if (await confirm.ShowAsync() != ContentDialogResult.Primary)
        {
            return;
        }

        var (ok, message) = await ViewModel.DeleteProfileAsync(row.Id);
        if (!ok)
        {
            await ShowErrorAsync("Couldn't delete the profile", message);
            return;
        }
        await ViewModel.RefreshAsync();
    }

    private async void ActiveToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (sender is not ToggleSwitch toggle ||
            toggle.DataContext is not ProfileRowViewModel row)
        {
            return;
        }

        // Skip the toggle raised while the template applies the bound value.
        if (row.Active == toggle.IsOn)
        {
            return;
        }

        bool desired = toggle.IsOn;

        // Activating: confirm and show exactly which rules it turns on.
        if (desired)
        {
            var names = ViewModel.MemberRuleNames(row.Profile.RuleIds);
            var body = names.Count == 0
                ? "This profile has no member rules, so activating it only applies its power mode."
                : "Activating this profile will turn on these rules:\n\n• " + string.Join("\n• ", names);
            if (!string.IsNullOrEmpty(row.PowerModeLabel) && row.PowerModeLabel != "No power-mode change")
            {
                body += $"\n\nIt will also set the power mode to {row.PowerModeLabel}.";
            }

            var confirm = new ContentDialog
            {
                XamlRoot = XamlRoot,
                Title = $"Activate “{row.Name}”?",
                Content = body,
                PrimaryButtonText = "Activate",
                CloseButtonText = "Cancel",
                DefaultButton = ContentDialogButton.Primary,
            };
            if (await confirm.ShowAsync() != ContentDialogResult.Primary)
            {
                toggle.IsOn = row.Active; // revert the visual toggle
                return;
            }
        }

        var (ok, message) = await ViewModel.SetActiveAsync(row.Id, desired);
        if (!ok)
        {
            toggle.IsOn = row.Active;
            await ShowErrorAsync("Couldn't change the profile", message);
            return;
        }
        row.ApplyActive(desired);
        await ViewModel.RefreshAsync();
    }

    private async Task CreateAsync(Atlas.V0.Profile profile)
    {
        if (_fake)
        {
            await ViewModel.RefreshAsync();
            return;
        }
        try
        {
            using var channel = Atlas.IpcClient.AtlasChannel.Connect(_who);
            var outcome = await channel.CreateProfileAsync(profile);
            if (!outcome.Supported)
            {
                await ShowErrorAsync("Couldn't create the profile", "This service is too old to manage profiles.");
                return;
            }
        }
        catch (Exception ex)
        {
            await ShowErrorAsync("Couldn't create the profile", ex.Message);
            return;
        }
        await ViewModel.RefreshAsync();
    }

    private async Task UpdateAsync(Atlas.V0.Profile profile)
    {
        if (_fake)
        {
            await ViewModel.RefreshAsync();
            return;
        }
        try
        {
            using var channel = Atlas.IpcClient.AtlasChannel.Connect(_who);
            var outcome = await channel.UpdateProfileAsync(profile);
            if (!outcome.Supported)
            {
                await ShowErrorAsync("Couldn't save the profile", "This service is too old to manage profiles.");
                return;
            }
            if (!outcome.Value.Ok)
            {
                await ShowErrorAsync("Couldn't save the profile", "The service rejected the change.");
                return;
            }
        }
        catch (Exception ex)
        {
            await ShowErrorAsync("Couldn't save the profile", ex.Message);
            return;
        }
        await ViewModel.RefreshAsync();
    }

    private async Task ShowErrorAsync(string title, string message)
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
