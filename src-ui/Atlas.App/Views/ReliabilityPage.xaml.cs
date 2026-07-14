using System;
using Atlas.App.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;

namespace Atlas.App.Views;

/// <summary>
/// The Reliability page (R3, PRD §9.14) — a calm history of crash / hang / bugcheck
/// / service-failure / unexpected-shutdown records over a selectable window, each
/// shown with its correlated context as hedged, correlation-not-blame bullets.
/// Honors two honest empty states — the transport "service too old" and the in-band
/// "reliability log unavailable" — and frames a genuinely empty window as good news
/// (the states live in the VM).
/// </summary>
public sealed partial class ReliabilityPage : Page
{
    public ReliabilityViewModel ViewModel { get; }

    public ReliabilityPage()
    {
        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        ViewModel = new ReliabilityViewModel(DispatcherQueue, string.IsNullOrEmpty(who) ? null : who);

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
