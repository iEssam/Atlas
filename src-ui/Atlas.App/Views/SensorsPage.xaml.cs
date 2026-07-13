using System;
using Atlas.App.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;

namespace Atlas.App.Views;

/// <summary>
/// The Sensors page (R2, PRD §9.6.6 / §9.6.7): Battery, Thermal, and Startup-history
/// cards. Each fetches independently and honours the honesty principle — an absent
/// battery, a machine that exposes no thermal sensors, or a missing boot log are all
/// stated plainly, never as errors. Degrades to a calm note when the service is too
/// old to serve a given RPC.
/// </summary>
public sealed partial class SensorsPage : Page
{
    public SensorsViewModel ViewModel { get; }

    public SensorsPage()
    {
        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        ViewModel = new SensorsViewModel(
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
