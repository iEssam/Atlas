using System;
using Atlas.App.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;

namespace Atlas.App.Views;

/// <summary>
/// The System Changes page (R3, PRD §9.13) — the product's headline "what changed?"
/// surface. A calm, scannable timeline (newest first) of recorded changes over a
/// selectable window, filterable by kind and free text. Selecting a row reveals its
/// full detail on the right. Degrades gracefully when the service is too old
/// (the honest "unavailable" state lives in the VM) and states an empty window
/// plainly — nothing changing is fine, not an error.
/// </summary>
public sealed partial class SystemChangesPage : Page
{
    public SystemChangesViewModel ViewModel { get; }

    public SystemChangesPage()
    {
        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        ViewModel = new SystemChangesViewModel(DispatcherQueue, string.IsNullOrEmpty(who) ? null : who);

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

    private void OnRefreshClick(object sender, RoutedEventArgs e) =>
        _ = ViewModel.RefreshAsync();
}
