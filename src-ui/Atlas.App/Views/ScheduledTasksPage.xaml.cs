using System;
using Atlas.App.ViewModels;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;

namespace Atlas.App.Views;

/// <summary>
/// The Scheduled Tasks page (R2, PRD §9.9.2): a virtualized, filterable table of
/// Windows scheduled tasks with a detail pane (action, author, run-level / idle /
/// wake flags) for the selected row. The filter debounces in the view-model and is
/// applied server-side. Read-only this milestone. Degrades to an inline placeholder
/// when the service is too old.
/// </summary>
public sealed partial class ScheduledTasksPage : Page
{
    public ScheduledTasksViewModel ViewModel { get; }

    public ScheduledTasksPage()
    {
        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        ViewModel = new ScheduledTasksViewModel(
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
