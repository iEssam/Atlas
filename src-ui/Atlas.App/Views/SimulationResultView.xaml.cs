using Atlas.App.ViewModels;
using Microsoft.UI.Xaml.Controls;

namespace Atlas.App.Views;

/// <summary>
/// A reusable control that renders a rule simulation (PRD §9.7.5): the affected
/// processes with their current→new priority/affinity/eco transitions, any
/// conflicts, and — clearly but calmly marked — the protected-critical targets
/// Atlas won't touch. Hosted both inside <see cref="RuleEditDialog"/> (preview
/// before committing) and in the standalone <see cref="SimulationPreviewDialog"/>,
/// so the impact rendering lives in exactly one place.
///
/// <para>
/// The <see cref="ViewModel"/> is assigned by the host after construction; we call
/// <c>Bindings.Update()</c> so the compiled x:Bind expressions pick it up.
/// </para>
/// </summary>
public sealed partial class SimulationResultView : UserControl
{
    private SimulationResultViewModel? _viewModel;

    public SimulationResultView()
    {
        InitializeComponent();
    }

    /// <summary>The simulation view-model. Setting it refreshes the bindings.</summary>
    public SimulationResultViewModel? ViewModel
    {
        get => _viewModel;
        set
        {
            _viewModel = value;
            Bindings.Update();
        }
    }
}
