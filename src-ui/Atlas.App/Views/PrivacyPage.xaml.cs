using System;
using Atlas.App.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;

namespace Atlas.App.Views;

/// <summary>
/// The Privacy page (M7, PRD §9.10): camera / microphone / location usage grouped
/// by capability, with an optional recent-activity list. Calm and factual — never
/// implies malice. Degrades to an inline placeholder when the service is too old.
/// </summary>
public sealed partial class PrivacyPage : Page
{
    public PrivacyViewModel ViewModel { get; }

    public PrivacyPage()
    {
        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        ViewModel = new PrivacyViewModel(
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
