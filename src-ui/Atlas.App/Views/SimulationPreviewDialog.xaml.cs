using Atlas.App.ViewModels;
using Atlas.IpcClient;
using Atlas.V0;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml.Controls;

namespace Atlas.App.Views;

/// <summary>
/// A read-only, standalone simulation preview (PRD §9.7.5) for an existing rule,
/// reachable from the Rules list. It hosts the shared
/// <see cref="SimulationResultView"/> and runs the pure dry-run on open — so the
/// user can, at any time, see exactly what a saved rule is doing (or would do) to
/// running processes, protected-critical targets included. Nothing is applied.
/// </summary>
public sealed partial class SimulationPreviewDialog : ContentDialog
{
    private readonly SimulationResultViewModel _sim;
    private readonly Rule _rule;

    public SimulationPreviewDialog(Rule rule, string? who, bool fake)
    {
        InitializeComponent();

        _rule = rule;
        _sim = new SimulationResultViewModel(DispatcherQueue, who, fake);
        SimView.ViewModel = _sim;

        SubtitleText.Text = $"What “{RulesFormatter.RuleSummary(rule)}” would change right now.";
        Opened += OnOpened;
    }

    private async void OnOpened(ContentDialog sender, ContentDialogOpenedEventArgs args)
    {
        await _sim.RunAsync(_rule);
    }
}
