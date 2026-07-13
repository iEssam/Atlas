using System;
using Atlas.App.ViewModels;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;

namespace Atlas.App.Views;

/// <summary>
/// The Search page (M6): one query, grouped hits (processes / events /
/// bookmarks). Degrades to an inline placeholder when the service is too old to
/// answer search.
/// </summary>
public sealed partial class SearchPage : Page
{
    public SearchViewModel ViewModel { get; }

    public SearchPage()
    {
        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        ViewModel = new SearchViewModel(
            DispatcherQueue, string.IsNullOrEmpty(who) ? null : who);

        InitializeComponent();
    }

    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        base.OnNavigatedFrom(e);
        ViewModel.Stop();
    }

    private void SearchBox_QuerySubmitted(
        AutoSuggestBox sender, AutoSuggestBoxQuerySubmittedEventArgs args)
    {
        _ = ViewModel.SearchAsync();
    }
}
