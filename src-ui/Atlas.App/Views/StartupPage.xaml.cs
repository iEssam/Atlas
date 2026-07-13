using System;
using Atlas.App.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;

namespace Atlas.App.Views;

/// <summary>
/// The Startup page (M7, PRD §9.8.1): the auto-start inventory grouped by source
/// (run keys / startup folders / tasks / services / packaged). Read-only this
/// milestone. Degrades to an inline placeholder when the service is too old.
/// </summary>
public sealed partial class StartupPage : Page
{
    public StartupViewModel ViewModel { get; }

    public StartupPage()
    {
        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        ViewModel = new StartupViewModel(
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

    private void Refresh_Click(object sender, RoutedEventArgs e) => _ = ViewModel.RefreshAsync();
}
