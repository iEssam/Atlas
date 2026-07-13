using System;
using Atlas.App.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;

namespace Atlas.App.Views;

/// <summary>
/// The Network page (R2, PRD §9.12): a virtualized, filterable table of active
/// connections (app/PID, protocol, endpoints, resolved domain, colored TCP state)
/// with a Listening-ports sub-view toggle. The filter debounces in the view-model
/// and is applied client-side. Read-only this milestone. Degrades to an inline
/// placeholder when the service is too old.
/// </summary>
public sealed partial class NetworkPage : Page
{
    public NetworkViewModel ViewModel { get; }

    public NetworkPage()
    {
        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        ViewModel = new NetworkViewModel(
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

    // The radio buttons drive the sub-view. Setting ShowListening re-queries via the
    // view-model's OnShowListeningChanged; guarding on the current value avoids a
    // redundant refresh when the binding re-checks the already-active button.
    private void Connections_Checked(object sender, RoutedEventArgs e)
    {
        if (ViewModel.ShowListening)
        {
            ViewModel.ShowListening = false;
        }
    }

    private void Listening_Checked(object sender, RoutedEventArgs e)
    {
        if (!ViewModel.ShowListening)
        {
            ViewModel.ShowListening = true;
        }
    }

    private void Refresh_Click(object sender, RoutedEventArgs e) => _ = ViewModel.RefreshAsync();
}
