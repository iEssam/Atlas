using Atlas.App.ViewModels;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;

namespace Atlas.App.Views;

/// <summary>
/// The Live Activity page: system gauges header + a virtualized process table
/// streaming ~1 Hz from the service. The pipe discriminator can be overridden
/// via the <c>ATLAS_PIPE</c> environment variable (else the USERNAME default).
/// </summary>
public sealed partial class LiveActivityPage : Page
{
    public LiveActivityViewModel ViewModel { get; }

    // Derived display strings so the header formats without a converter.
    public string CapabilitiesText =>
        $"service v{ViewModel.ServiceVersion}  •  capabilities: {ViewModel.CapabilityFlags}";
    public string CpuText => $"{ViewModel.SystemCpuPercent:F1} %";
    public string MemText => $"{ViewModel.MemUsedGb:F1} / {ViewModel.MemTotalGb:F1} GB";

    public LiveActivityPage()
    {
        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        ViewModel = new LiveActivityViewModel(
            DispatcherQueue,
            string.IsNullOrEmpty(who) ? null : who);

        InitializeComponent();

        // Refresh derived header strings whenever any underlying VM property
        // they depend on changes (capabilities line + CPU/memory gauge text).
        ViewModel.PropertyChanged += (_, e) =>
        {
            if (e.PropertyName is nameof(ViewModel.ServiceVersion)
                or nameof(ViewModel.CapabilityFlags)
                or nameof(ViewModel.SystemCpuPercent)
                or nameof(ViewModel.MemUsedGb)
                or nameof(ViewModel.MemTotalGb))
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
