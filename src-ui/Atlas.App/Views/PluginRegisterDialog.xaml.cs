using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using Atlas.App.ViewModels;
using Atlas.IpcClient;
using Atlas.V0;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.Storage;
using Windows.Storage.Pickers;

namespace Atlas.App.Views;

/// <summary>
/// The plugin registration dialog (PRD §18.3). The user picks a plugin executable
/// (via a <see cref="FileOpenPicker"/> initialized with the owner window, per the
/// M8 report file-picker pattern), ticks which read-only capability groups to grant,
/// and — only if they explicitly opt in behind a clear caution — allows an unsigned
/// executable. It then calls RegisterPlugin through the shared view-model, which
/// refreshes the list on success and surfaces the server's refusal message
/// (e.g. "refused: executable is not signed") inline when it declines.
///
/// <para>
/// The framing is the point: a plugin is separate, read-only, and off until enabled;
/// it gets only the capabilities ticked here. Unsigned is a caution, not a threat.
/// Against a service too old to serve RegisterPlugin (Unimplemented → Unsupported)
/// the dialog shows a calm "unavailable" note instead of crashing.
/// </para>
/// </summary>
public sealed partial class PluginRegisterDialog : ContentDialog
{
    private readonly PluginsViewModel _viewModel;

    /// <summary>The seven grantable read-only capability groups, none ticked by default.</summary>
    public ObservableCollection<CapabilityChoiceViewModel> Capabilities { get; } = new();

    public PluginRegisterDialog(PluginsViewModel viewModel)
    {
        _viewModel = viewModel;

        foreach (var cap in PluginFormatter.AllCapabilities)
        {
            Capabilities.Add(new CapabilityChoiceViewModel(cap, selected: false));
        }

        InitializeComponent();

        CapabilityList.ItemsSource = Capabilities;
    }

    private async void OnBrowseClick(object sender, RoutedEventArgs e)
    {
        try
        {
            var picker = new FileOpenPicker
            {
                SuggestedStartLocation = PickerLocationId.ComputerFolder,
            };
            picker.FileTypeFilter.Add(".exe");

            // Unpackaged WinUI: the picker needs an owner HWND or it throws.
            var window = App.MainWindow;
            if (window is null)
            {
                ShowStatus(InfoBarSeverity.Warning, "Can't open the file dialog",
                    "Type or paste the executable path is not supported here — try again from the main window.");
                return;
            }
            var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(window);
            WinRT.Interop.InitializeWithWindow.Initialize(picker, hwnd);

            var file = await picker.PickSingleFileAsync();
            if (file is null)
            {
                return; // user cancelled
            }
            PathBox.Text = file.Path;
            StatusBar.IsOpen = false;
        }
        catch (Exception ex)
        {
            ShowStatus(InfoBarSeverity.Warning, "Couldn't open the file dialog", ex.Message);
        }
    }

    private async void OnRegisterClick(object sender, RoutedEventArgs e)
    {
        var path = PathBox.Text?.Trim() ?? string.Empty;
        if (path.Length == 0)
        {
            ShowStatus(InfoBarSeverity.Warning, "Choose an executable",
                "Pick the plugin's .exe first, then choose the capabilities to grant.");
            return;
        }

        var granted = SelectedCapabilities();
        bool allowUnsigned = AllowUnsignedCheck.IsChecked == true;

        StatusBar.IsOpen = false;
        SetBusy(true);
        try
        {
            var (ok, message) = await _viewModel.RegisterPluginAsync(path, granted, allowUnsigned);
            SetBusy(false);

            if (!ok)
            {
                // A refusal (e.g. unsigned + not allowed) is expected, not a crash —
                // surface it plainly, with a nudge toward the opt-in when relevant.
                ShowStatus(InfoBarSeverity.Warning, "Not registered",
                    string.IsNullOrEmpty(message) ? "The plugin could not be registered." : message);
                return;
            }

            ShowStatus(InfoBarSeverity.Success, "Registered",
                string.IsNullOrEmpty(message)
                    ? "The plugin was registered. It stays off until you enable it."
                    : message);
            RegisterButton.IsEnabled = false;
        }
        catch (Exception ex)
        {
            SetBusy(false);
            ShowStatus(InfoBarSeverity.Error, "Couldn't register the plugin", ex.Message);
        }
    }

    private IReadOnlyList<PluginCapability> SelectedCapabilities() =>
        Capabilities.Where(c => c.IsSelected).Select(c => c.Capability).ToList();

    private void SetBusy(bool busy)
    {
        BusyRing.IsActive = busy;
        RegisterButton.IsEnabled = !busy;
        BrowseButton.IsEnabled = !busy;
    }

    private void ShowStatus(InfoBarSeverity severity, string title, string message)
    {
        StatusBar.Severity = severity;
        StatusBar.Title = title;
        StatusBar.Message = message;
        StatusBar.IsOpen = true;
    }
}
