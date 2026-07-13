using System;
using Atlas.App.ViewModels;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Navigation;
using Windows.Storage.Pickers;
using Windows.System;

namespace Atlas.App.Views;

/// <summary>
/// The File-Lock Search page — "find what is using this file" (R2, PRD §9.5). A
/// path input (with a <see cref="FileOpenPicker"/> Browse, initialized with the
/// owner HWND per the M8 report-dialog pattern) drives FindResourceOwners and
/// lists the owning processes, each with an Inspect link into the Process
/// Inspector. Degrades gracefully when the service is too old and distinguishes
/// "path not available" from "nothing is holding the file" (logic in the VM).
/// </summary>
public sealed partial class FileLockPage : Page
{
    private readonly string? _who;

    public FileLockViewModel ViewModel { get; }

    public FileLockPage()
    {
        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        _who = string.IsNullOrEmpty(who) ? null : who;
        ViewModel = new FileLockViewModel(DispatcherQueue, _who);

        InitializeComponent();
    }

    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        base.OnNavigatedFrom(e);
        ViewModel.Stop();
    }

    private void OnSearchClick(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        _ = ViewModel.SearchAsync();

    private void OnPathKeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key == VirtualKey.Enter)
        {
            e.Handled = true;
            _ = ViewModel.SearchAsync();
        }
    }

    /// <summary>
    /// Browse for a file to check. Uses the same unpackaged-WinUI picker pattern
    /// as the M8 report dialog: the picker needs an owner HWND or it throws, so
    /// we initialize it with the main window's handle.
    /// </summary>
    private async void OnBrowseClick(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        try
        {
            var picker = new FileOpenPicker
            {
                SuggestedStartLocation = PickerLocationId.ComputerFolder,
            };
            picker.FileTypeFilter.Add("*");

            var window = App.MainWindow;
            if (window is null)
            {
                return; // no owner window to initialize the picker against
            }
            var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(window);
            WinRT.Interop.InitializeWithWindow.Initialize(picker, hwnd);

            var file = await picker.PickSingleFileAsync();
            if (file is not null)
            {
                ViewModel.PathInput = file.Path;
                _ = ViewModel.SearchAsync();
            }
        }
        catch
        {
            // Picker fiddliness under the unpackaged host: the user can still
            // paste a path into the box and search.
        }
    }

    /// <summary>Opens the Process Inspector for a file's owning process.</summary>
    private void OnInspectOwnerClick(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (sender is not Button { DataContext: ResourceOwnerItem owner })
        {
            return;
        }

        // Resource owners carry no create_time; pass 0 for best-effort by pid
        // (the server's ProcessDetailRequest documents 0 = best-effort).
        var inspector = new InspectorWindow(_who, owner.Pid, 0, owner.ImageName);
        inspector.Activate();
    }
}
