using Atlas.App.ViewModels;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;

namespace Atlas.App.Views;

public sealed partial class GpuPage : Page
{
    public GpuViewModel ViewModel { get; }
    public GpuPage()
    {
        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        ViewModel = new GpuViewModel(DispatcherQueue, string.IsNullOrEmpty(who) ? null : who);
        InitializeComponent();
    }
    protected override void OnNavigatedTo(NavigationEventArgs e) { base.OnNavigatedTo(e); ViewModel.Start(); }
    protected override void OnNavigatedFrom(NavigationEventArgs e) { ViewModel.Stop(); base.OnNavigatedFrom(e); }
}
