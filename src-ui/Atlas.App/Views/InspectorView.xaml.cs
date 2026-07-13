using Atlas.App.ViewModels;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace Atlas.App.Views;

/// <summary>
/// The Process Inspector content (R2, PRD §9.4) — a four-tab <see cref="TabView"/>
/// (Overview / Handles / Modules / Threads) over <see cref="InspectorViewModel"/>.
/// Hosted inside <see cref="InspectorWindow"/>. It lives in a
/// <see cref="UserControl"/> (a <c>FrameworkElement</c>) rather than directly in
/// the Window so <c>x:Bind</c> converters resolve — a Window is not a
/// <c>FrameworkElement</c> and can't be a converter-lookup root.
///
/// <para>
/// Tabs load <b>lazily</b>: the selected tab loads on first view (Overview loads
/// on open), so opening the Inspector never fires all four RPCs at once. Each tab
/// has its own refresh. Coverage limits and unavailable states are surfaced
/// honestly by the view-model.
/// </para>
/// </summary>
public sealed partial class InspectorView : UserControl
{
    public InspectorViewModel ViewModel { get; }

    public InspectorView(string? who, uint pid, long createTime100ns, string imageName)
    {
        ViewModel = new InspectorViewModel(
            DispatcherQueue, who, pid, createTime100ns, imageName);

        InitializeComponent();

        // Overview is the default tab — load it eagerly (still just one RPC).
        _ = ViewModel.EnsureOverviewAsync();
    }

    /// <summary>Lazy-loads the newly-selected tab's data on first view.</summary>
    private void OnTabSelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (Tabs.SelectedItem is not TabViewItem item)
        {
            return;
        }

        switch (item.Header as string)
        {
            case "Overview":
                _ = ViewModel.EnsureOverviewAsync();
                break;
            case "Handles":
                _ = ViewModel.EnsureHandlesAsync();
                break;
            case "Modules":
                _ = ViewModel.EnsureModulesAsync();
                break;
            case "Threads":
                _ = ViewModel.EnsureThreadsAsync();
                break;
        }
    }

    private void OnRefreshOverview(object sender, RoutedEventArgs e) =>
        _ = ViewModel.RefreshOverviewAsync();

    private void OnRefreshHandles(object sender, RoutedEventArgs e) =>
        _ = ViewModel.RefreshHandlesAsync();

    private void OnRefreshModules(object sender, RoutedEventArgs e) =>
        _ = ViewModel.RefreshModulesAsync();

    private void OnRefreshThreads(object sender, RoutedEventArgs e) =>
        _ = ViewModel.RefreshThreadsAsync();
}
