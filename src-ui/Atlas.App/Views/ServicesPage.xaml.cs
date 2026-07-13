using System;
using Atlas.App.ViewModels;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;

namespace Atlas.App.Views;

/// <summary>
/// The Services page (M7, PRD §9.9.1): a virtualized, filterable table of Windows
/// services with a detail pane (binary path + description) for the selected row.
/// The filter debounces in the view-model. Read-only this milestone. Degrades to
/// an inline placeholder when the service is too old.
/// </summary>
public sealed partial class ServicesPage : Page
{
    public ServicesViewModel ViewModel { get; }

    public ServicesPage()
    {
        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        ViewModel = new ServicesViewModel(
            DispatcherQueue, string.IsNullOrEmpty(who) ? null : who);

        InitializeComponent();
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
}
