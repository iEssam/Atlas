using System.Xml.Linq;
using Xunit;

namespace Atlas.App.Tests;

public sealed class GamingWorkspaceContractTests
{
    private static string RepositoryRoot
    {
        get
        {
            var directory = new DirectoryInfo(AppContext.BaseDirectory);
            while (directory is not null)
            {
                if (File.Exists(Path.Combine(directory.FullName, "Cargo.toml"))) return directory.FullName;
                directory = directory.Parent;
            }
            throw new DirectoryNotFoundException("Could not locate repository root.");
        }
    }

    [Fact]
    public void GamingIsADirectDestinationImmediatelyAfterOverview()
    {
        var document = XDocument.Load(Path.Combine(RepositoryRoot, "src-ui", "Atlas.App", "MainWindow.xaml"));
        var items = document.Descendants().Where(element => element.Name.LocalName == "NavigationViewItem").ToArray();
        var overview = Array.FindIndex(items, item => item.Attribute("Tag")?.Value == "overview");
        var gaming = Array.FindIndex(items, item => item.Attribute("Tag")?.Value == "gaming");
        Assert.True(overview >= 0);
        Assert.Equal(overview + 1, gaming);
    }

    [Fact]
    public void WorkspaceUsesNativeControlsAndOneAccessibleProofTrack()
    {
        var xaml = File.ReadAllText(Path.Combine(RepositoryRoot, "src-ui", "Atlas.App", "Views", "GamingPage.xaml"));
        var proof = File.ReadAllText(Path.Combine(RepositoryRoot, "src-ui", "Atlas.App", "Controls", "GamingProofTrack.xaml"));
        Assert.Contains("<InfoBar", xaml, StringComparison.Ordinal);
        Assert.Contains("<CommandBar", xaml, StringComparison.Ordinal);
        Assert.Contains("<ListView", xaml, StringComparison.Ordinal);
        Assert.Contains("<controls:GamingProofTrack", xaml, StringComparison.Ordinal);
        Assert.Contains("Accessible trace summary", xaml, StringComparison.Ordinal);
        Assert.Contains("AutomationProperties.HelpText", proof, StringComparison.Ordinal);
        Assert.DoesNotContain("GradientBrush", xaml, StringComparison.Ordinal);
        Assert.DoesNotContain("DropShadow", xaml, StringComparison.Ordinal);
        Assert.DoesNotContain("TranslateTransform", xaml, StringComparison.Ordinal);
    }

    [Fact]
    public void RecordedSessionSelectionUsesTheDisplayedSessionTypeAndLoadsFromTheViewModel()
    {
        var xaml = File.ReadAllText(Path.Combine(RepositoryRoot, "src-ui", "Atlas.App", "Views", "GamingPage.xaml"));
        var viewModel = File.ReadAllText(Path.Combine(RepositoryRoot, "src-ui", "Atlas.App", "ViewModels", "GamingViewModel.cs"));

        Assert.Contains("SelectedItem=\"{x:Bind ViewModel.SelectedSession, Mode=TwoWay}\"", xaml, StringComparison.Ordinal);
        Assert.DoesNotContain("SelectionChanged=\"Session_SelectionChanged\"", xaml, StringComparison.Ordinal);
        Assert.Contains("private GamingSessionDisplay? _selectedSession", viewModel, StringComparison.Ordinal);
        Assert.Contains("partial void OnSelectedSessionChanged(GamingSessionDisplay? value)", viewModel, StringComparison.Ordinal);
        Assert.Contains("SelectedSession = Sessions.FirstOrDefault", viewModel, StringComparison.Ordinal);
    }

    [Fact]
    public void SessionPerformanceIsMeasuredExplainedAndNeverEstimatedFromUtilization()
    {
        var xaml = File.ReadAllText(Path.Combine(RepositoryRoot, "src-ui", "Atlas.App", "Views", "GamingPage.xaml"));
        var viewModel = File.ReadAllText(Path.Combine(RepositoryRoot, "src-ui", "Atlas.App", "ViewModels", "GamingViewModel.cs"));
        var proof = File.ReadAllText(Path.Combine(RepositoryRoot, "src-ui", "Atlas.App", "Controls", "GamingProofTrack.xaml"));
        var proofCode = File.ReadAllText(Path.Combine(RepositoryRoot, "src-ui", "Atlas.App", "Controls", "GamingProofTrack.xaml.cs"));

        Assert.Contains("1% low", xaml, StringComparison.Ordinal);
        Assert.Contains("Frame p95", xaml, StringComparison.Ordinal);
        Assert.Contains("Long frames", xaml, StringComparison.Ordinal);
        Assert.Contains("PerformanceExplanation", xaml, StringComparison.Ordinal);
        Assert.Contains("Frame time was not captured for this recording", proof, StringComparison.Ordinal);
        Assert.Contains("Frame time p95 by second", proofCode, StringComparison.Ordinal);
        Assert.Contains("Atlas never estimates FPS from CPU or GPU utilization.", viewModel, StringComparison.Ordinal);
        Assert.Contains("diagnostic until anti-cheat compatibility", viewModel, StringComparison.Ordinal);
    }

    [Fact]
    public void PlanConfirmationSurfacesBeforeAfterRollbackAndVerification()
    {
        var code = File.ReadAllText(Path.Combine(RepositoryRoot, "src-ui", "Atlas.App", "Views", "GamingPage.xaml.cs"));
        Assert.Contains("Plan hash:", code, StringComparison.Ordinal);
        Assert.Contains("Before:", code, StringComparison.Ordinal);
        Assert.Contains("After:", code, StringComparison.Ordinal);
        Assert.Contains("Rollback:", code, StringComparison.Ordinal);
        Assert.Contains("Verification:", code, StringComparison.Ordinal);
        Assert.Contains("GamingRiskLane.AutomaticReversible", code, StringComparison.Ordinal);
    }

    [Fact]
    public void GamingMutationClientRemainsSeparateFromGamingQueryClient()
    {
        var client = File.ReadAllText(Path.Combine(RepositoryRoot, "src-ui", "Atlas.IpcClient", "AtlasChannel.cs"));
        Assert.Contains("AtlasGamingQuery.AtlasGamingQueryClient _gamingQuery", client, StringComparison.Ordinal);
        Assert.Contains("AtlasGamingControl.AtlasGamingControlClient _gamingControl", client, StringComparison.Ordinal);
        Assert.Contains("_gamingQuery.ListDetectedGamesAsync", client, StringComparison.Ordinal);
        Assert.Contains("_gamingControl.ExecuteGamingPlanAsync", client, StringComparison.Ordinal);
        Assert.Contains("_gamingControl.KeepGamingPlanAsync", client, StringComparison.Ordinal);
    }
}
