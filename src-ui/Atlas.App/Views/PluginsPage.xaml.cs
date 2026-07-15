using System;
using Atlas.App.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;

namespace Atlas.App.Views;

/// <summary>
/// The Plugins page (R3, PRD §18.3): the registry of signed, out-of-process,
/// capability-scoped <b>read-only</b> extensions. Lists each plugin with its
/// signature badge, granted read-only capabilities (as chips), an enabled toggle,
/// and per-row edit-capabilities / remove; the "Register a plugin" button opens
/// <see cref="PluginRegisterDialog"/> (file picker + capability grant + gated
/// unsigned opt-in), and edit-capabilities opens <see cref="PluginGrantDialog"/>.
///
/// <para>
/// The security framing is deliberate and calm: read-only, signed, per-capability
/// grant, off by default — unmistakable but never alarmist, and unsigned is a
/// caution, not a threat. The whole page degrades gracefully when AtlasPlugins is
/// unavailable (it lands after this UI). Set <c>ATLAS_FAKE_PLUGINS=1</c> to explore
/// the UX with demo data.
/// </para>
/// </summary>
public sealed partial class PluginsPage : Page
{
    private readonly string? _who;
    private readonly bool _fake;

    public PluginsViewModel ViewModel { get; }

    public PluginsPage()
    {
        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        _who = string.IsNullOrEmpty(who) ? null : who;
        _fake = Environment.GetEnvironmentVariable("ATLAS_FAKE_PLUGINS") == "1";

        ViewModel = new PluginsViewModel(DispatcherQueue, _who, _fake);
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

    private async void RegisterPlugin_Click(object sender, RoutedEventArgs e)
    {
        var dialog = new PluginRegisterDialog(ViewModel) { XamlRoot = XamlRoot };
        await dialog.ShowAsync();
        // The dialog registers through the view-model (which refreshes on success),
        // so there's nothing more to do here — the list already reflects the result.
    }

    private async void EditGrant_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not PluginRowViewModel row)
        {
            return;
        }

        var dialog = new PluginGrantDialog(row.Name, row.GrantedCapabilities) { XamlRoot = XamlRoot };
        var result = await dialog.ShowAsync();
        if (result != ContentDialogResult.Primary || dialog.ResultCapabilities is null)
        {
            return;
        }

        var (ok, message) = await ViewModel.GrantCapabilitiesAsync(row.Id, dialog.ResultCapabilities);
        if (!ok)
        {
            await ShowErrorAsync("Couldn't update capabilities", message);
        }
    }

    private async void RemovePlugin_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not PluginRowViewModel row)
        {
            return;
        }

        var confirm = new ContentDialog
        {
            XamlRoot = XamlRoot,
            Title = "Remove plugin?",
            Content = $"“{row.Name}” will be removed from the registry. If it's enabled it will be stopped, and it loses all granted access. You can register it again later.",
            PrimaryButtonText = "Remove",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
        };
        if (await confirm.ShowAsync() != ContentDialogResult.Primary)
        {
            return;
        }

        var (ok, message) = await ViewModel.RemovePluginAsync(row.Id);
        if (!ok)
        {
            await ShowErrorAsync("Couldn't remove the plugin", message);
            return;
        }
        await ViewModel.RefreshAsync();
    }

    private async void EnabledToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (sender is not ToggleSwitch toggle ||
            toggle.DataContext is not PluginRowViewModel row)
        {
            return;
        }

        // Skip the toggle event raised while the template applies the bound initial
        // value — only act on a genuine user-driven change.
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
            await ShowErrorAsync("Couldn't change the plugin", message);
        }
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
