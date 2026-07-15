using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using Atlas.App.ViewModels;
using Atlas.IpcClient;
using Atlas.V0;
using Microsoft.UI.Xaml.Controls;

namespace Atlas.App.Views;

/// <summary>
/// The per-plugin grant-editing dialog (PRD §18.3): re-grant which read-only
/// capability groups a plugin may read. Seeded from the plugin's current grant; on
/// Save it exposes the chosen set via <see cref="ResultCapabilities"/> for the page
/// to commit through GrantPluginCapabilities.
///
/// <para>
/// Capability framing is the whole point — each toggle is a read-only slice, and the
/// plugin gets ONLY what is ticked here (including nothing, which is a valid, safe
/// choice that leaves it with no access).
/// </para>
/// </summary>
public sealed partial class PluginGrantDialog : ContentDialog
{
    /// <summary>The seven capability groups, pre-ticked to the plugin's current grant.</summary>
    public ObservableCollection<CapabilityChoiceViewModel> Capabilities { get; } = new();

    /// <summary>The chosen capability set after Save (null if cancelled).</summary>
    public IReadOnlyList<PluginCapability>? ResultCapabilities { get; private set; }

    public PluginGrantDialog(string pluginName, IReadOnlyList<PluginCapability> currentlyGranted)
    {
        var grantedSet = new HashSet<PluginCapability>(
            PluginFormatter.NormalizeCapabilities(currentlyGranted));

        foreach (var cap in PluginFormatter.AllCapabilities)
        {
            Capabilities.Add(new CapabilityChoiceViewModel(cap, grantedSet.Contains(cap)));
        }

        InitializeComponent();

        HeaderText.Text = $"Capabilities for “{pluginName}”";
        CapabilityList.ItemsSource = Capabilities;
        PrimaryButtonClick += OnPrimaryButtonClick;
    }

    private void OnPrimaryButtonClick(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        ResultCapabilities = Capabilities
            .Where(c => c.IsSelected)
            .Select(c => c.Capability)
            .ToList();
    }
}
