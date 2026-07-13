using System;
using Atlas.App.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;

namespace Atlas.App.Views;

/// <summary>
/// The Diagnostics page (M8, PRD §9.15.2) — the milestone centerpiece. A left rail
/// lists detected incidents over a selectable window; selecting one runs Diagnose
/// and renders the full structured explanation on the right. "Diagnose current
/// window" covers the ad-hoc (no-incident) case, and "Export report" opens the
/// report dialog. Every surface degrades gracefully when the service is too old or
/// declines for want of evidence (the honest, first-class states live in the VM).
/// </summary>
public sealed partial class DiagnosticsPage : Page
{
    private readonly string? _who;

    public DiagnosticsViewModel ViewModel { get; }

    public DiagnosticsPage()
    {
        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        _who = string.IsNullOrEmpty(who) ? null : who;
        ViewModel = new DiagnosticsViewModel(DispatcherQueue, _who);

        InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        _ = ViewModel.RefreshIncidentsAsync();
    }

    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        base.OnNavigatedFrom(e);
        ViewModel.Stop();
    }

    private void OnRefreshClick(object sender, RoutedEventArgs e) =>
        _ = ViewModel.RefreshIncidentsAsync();

    private void OnDiagnoseWindowClick(object sender, RoutedEventArgs e) =>
        _ = ViewModel.DiagnoseCurrentWindowAsync();

    private async void OnExportClick(object sender, RoutedEventArgs e)
    {
        if (!ViewModel.CanExportReport)
        {
            return;
        }

        var dialog = new ReportDialog(
            _who,
            ViewModel.CurrentIncidentId,
            ViewModel.CurrentFromMs,
            ViewModel.CurrentToMs)
        {
            XamlRoot = XamlRoot,
        };
        await dialog.ShowAsync();
    }
}
