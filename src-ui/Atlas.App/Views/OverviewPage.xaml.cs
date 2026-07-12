using Atlas.App.ViewModels;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;

namespace Atlas.App.Views;

/// <summary>
/// The Overview page: a card row of system gauges (CPU, memory with progress
/// bar, commit, process/thread/handle counts) plus a top-5 "Top consumers"
/// list. Same ring-preferred data source as Live Activity. The pipe/ring
/// discriminator can be overridden via the <c>ATLAS_PIPE</c> environment
/// variable (else the USERNAME default).
/// </summary>
public sealed partial class OverviewPage : Page
{
    public OverviewViewModel ViewModel { get; }

    /// <summary>Commit sub-line under the memory card.</summary>
    public string CommitLine => $"Commit {ViewModel.CommitText}";

    public OverviewPage()
    {
        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        ViewModel = new OverviewViewModel(
            DispatcherQueue,
            string.IsNullOrEmpty(who) ? null : who);

        InitializeComponent();

        // Refresh the derived commit line whenever its underlying VM strings
        // change (the x:Bind gauges refresh themselves via their own change
        // notifications).
        ViewModel.PropertyChanged += (_, e) =>
        {
            if (e.PropertyName is nameof(ViewModel.CommitText))
            {
                DispatcherQueue.TryEnqueue(() => Bindings.Update());
            }
        };
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        ViewModel.Start();
    }

    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        base.OnNavigatedFrom(e);
        ViewModel.Stop();
    }
}
