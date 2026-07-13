using System;
using Atlas.App.ViewModels;
using Atlas.IpcClient;
using Atlas.V0;
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

    /// <summary>
    /// "Inspect" context-menu item: opens the Process Inspector for this row.
    /// </summary>
    private void ProcessInspect_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (sender is MenuFlyoutItem { DataContext: ProcessRowViewModel row })
        {
            OpenInspector(row);
        }
    }

    /// <summary>Double-clicking a process row opens the Inspector (PRD §9.4).</summary>
    private void ProcessRow_DoubleTapped(
        object sender, Microsoft.UI.Xaml.Input.DoubleTappedRoutedEventArgs e)
    {
        if (sender is Microsoft.UI.Xaml.FrameworkElement { DataContext: ProcessRowViewModel row })
        {
            OpenInspector(row);
        }
    }

    /// <summary>
    /// Opens a standalone Inspector window for a process, keyed by its (pid,
    /// create_time) identity so the server can guard against PID reuse.
    /// </summary>
    private void OpenInspector(ProcessRowViewModel row)
    {
        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        var inspector = new InspectorWindow(
            string.IsNullOrEmpty(who) ? null : who,
            row.Pid,
            row.CreateTime100ns,
            row.ImageName);
        inspector.Activate();
    }

    /// <summary>
    /// Right-click context menu on a process row: Close / Suspend / Resume /
    /// End. Opens the two-phase safe-action dialog (PRD §9.22). The real broker
    /// call goes over the live channel and degrades to "unavailable" until the
    /// backend lands; setting <c>ATLAS_FAKE_BROKER=1</c> drives the dialog from a
    /// <see cref="FakeActionBroker"/> so the allowed→execute UX can be exercised
    /// without a live broker.
    /// </summary>
    private async void ProcessAction_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (sender is not MenuFlyoutItem item ||
            item.DataContext is not ProcessRowViewModel row)
        {
            return;
        }

        var action = (item.Tag as string) switch
        {
            "close" => ProcessActionKind.CloseWindows,
            "suspend" => ProcessActionKind.Suspend,
            "resume" => ProcessActionKind.Resume,
            "terminate" => ProcessActionKind.Terminate,
            _ => ProcessActionKind.ProcessActionUnspecified,
        };
        if (action == ProcessActionKind.ProcessActionUnspecified)
        {
            return;
        }

        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        var useFake = Environment.GetEnvironmentVariable("ATLAS_FAKE_BROKER") == "1";

        IActionBroker broker;
        AtlasChannel? channel = null;
        if (useFake)
        {
            broker = BuildDemoBroker(action);
        }
        else
        {
            channel = AtlasChannel.Connect(string.IsNullOrEmpty(who) ? null : who);
            broker = new ChannelActionBroker(channel);
        }

        try
        {
            var dialog = new SafeActionDialog(
                broker,
                row.Pid,
                row.CreateTime100ns,
                action,
                $"{row.ImageName} (pid {row.Pid})")
            {
                XamlRoot = XamlRoot,
            };
            await dialog.ShowAsync();
        }
        finally
        {
            channel?.Dispose();
        }
    }

    /// <summary>
    /// A design/demo broker so the allowed and denied UX can be seen without the
    /// backend: Terminate is denied (as if critical), everything else allowed
    /// with a representative risk picture.
    /// </summary>
    private static IActionBroker BuildDemoBroker(ProcessActionKind action)
    {
        if (action == ProcessActionKind.Terminate)
        {
            return FakeActionBroker.Denying(
                "This process is on the protected-critical list and cannot be ended.",
                new ActionRisk { IsCritical = true, IsSystem = true });
        }

        var risk = new ActionRisk { VisibleWindows = 2, ChildCount = 3 };
        risk.Notes.Add("Child processes will keep running (they become orphans).");
        risk.Notes.Add("Unsaved work in visible windows may be lost.");
        return FakeActionBroker.Allowing(risk, executeMessage: "The action completed.");
    }
}
