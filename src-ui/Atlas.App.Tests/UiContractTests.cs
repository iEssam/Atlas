using System.Text.RegularExpressions;
using System.Xml;
using System.Xml.Linq;
using Xunit;

namespace Atlas.App.Tests;

public sealed class UiContractTests
{
    private static readonly XNamespace Xaml = "http://schemas.microsoft.com/winfx/2006/xaml";
    private static readonly XNamespace Presentation = "http://schemas.microsoft.com/winfx/2006/xaml/presentation";

    private static readonly HashSet<string> EventAttributes =
    [
        "Checked", "Click", "Closing", "DoubleTapped", "DragOver", "Drop", "Invoked",
        "KeyDown", "Loaded", "PointerPressed", "PrimaryButtonClick", "QuerySubmitted",
        "SecondaryButtonClick", "SelectionChanged", "SizeChanged", "Tapped", "TextChanged",
        "Toggled", "Unchecked", "Opened", "ValueChanged",
    ];

    private static string RepositoryRoot
    {
        get
        {
            var directory = new DirectoryInfo(AppContext.BaseDirectory);
            while (directory is not null)
            {
                if (Directory.Exists(Path.Combine(directory.FullName, "src-ui", "Atlas.App"))
                    && File.Exists(Path.Combine(directory.FullName, "Cargo.toml")))
                {
                    return directory.FullName;
                }

                directory = directory.Parent;
            }

            throw new DirectoryNotFoundException(
                $"Could not locate the repository root above {AppContext.BaseDirectory}.");
        }
    }

    private static string AppRoot => Path.Combine(RepositoryRoot, "src-ui", "Atlas.App");

    public static IEnumerable<object[]> XamlFiles() =>
        Directory.EnumerateFiles(AppRoot, "*.xaml", SearchOption.AllDirectories)
            .Where(path => !path.Contains($"{Path.DirectorySeparatorChar}bin{Path.DirectorySeparatorChar}", StringComparison.OrdinalIgnoreCase))
            .Where(path => !path.Contains($"{Path.DirectorySeparatorChar}obj{Path.DirectorySeparatorChar}", StringComparison.OrdinalIgnoreCase))
            .Order(StringComparer.OrdinalIgnoreCase)
            .Select(path => new object[] { path });

    [Theory]
    [MemberData(nameof(XamlFiles))]
    public void XamlDocumentIsWellFormed(string path)
    {
        var document = XDocument.Load(path, LoadOptions.SetLineInfo);
        Assert.NotNull(document.Root);
    }

    [Theory]
    [MemberData(nameof(XamlFiles))]
    public void XamlEventHandlersExistInCodeBehind(string path)
    {
        var document = XDocument.Load(path, LoadOptions.SetLineInfo);
        var className = document.Root?.Attribute(Xaml + "Class")?.Value;
        var handlers = document.Root!.DescendantsAndSelf()
            .Attributes()
            .Where(attribute => EventAttributes.Contains(attribute.Name.LocalName))
            .Select(attribute => attribute.Value)
            .Where(value => Regex.IsMatch(value, "^[A-Za-z_][A-Za-z0-9_]*$"))
            .Distinct(StringComparer.Ordinal)
            .ToArray();

        if (handlers.Length == 0)
        {
            return;
        }

        Assert.False(string.IsNullOrWhiteSpace(className), $"{path} declares events but has no x:Class.");
        var codeBehindPath = path + ".cs";
        Assert.True(File.Exists(codeBehindPath), $"Missing code-behind for {path}.");
        var code = File.ReadAllText(codeBehindPath);

        foreach (var handler in handlers)
        {
            Assert.Matches($@"\b{Regex.Escape(handler)}\s*\(", code);
        }
    }

    [Fact]
    public void EveryAtlasStaticResourceReferenceResolves()
    {
        var documents = XamlFiles()
            .Select(row => (string)row[0])
            .Select(path => (Path: path, Document: XDocument.Load(path, LoadOptions.SetLineInfo)))
            .ToArray();
        var defined = documents
            .SelectMany(item => item.Document.Root!.DescendantsAndSelf())
            .Select(element => element.Attribute(Xaml + "Key")?.Value)
            .Where(key => key?.StartsWith("Atlas", StringComparison.Ordinal) is true)
            .Cast<string>()
            .ToHashSet(StringComparer.Ordinal);
        var unresolved = documents
            .SelectMany(item => item.Document.Root!.DescendantsAndSelf()
                .Attributes()
                .SelectMany(attribute => Regex.Matches(
                        attribute.Value,
                        @"\{StaticResource\s+(?<key>Atlas[A-Za-z0-9_.-]+)\}")
                    .Select(match => (item.Path, Key: match.Groups["key"].Value))))
            .Where(reference => !defined.Contains(reference.Key))
            .Select(reference => $"{Path.GetRelativePath(AppRoot, reference.Path)}: {reference.Key}")
            .Distinct(StringComparer.Ordinal)
            .Order(StringComparer.Ordinal)
            .ToArray();

        Assert.True(unresolved.Length == 0,
            $"Unresolved Atlas StaticResource references:{Environment.NewLine}{string.Join(Environment.NewLine, unresolved)}");
    }

    [Fact]
    public void MainWindowNavigationPageReferencesResolveToXamlViews()
    {
        var mainWindowCode = File.ReadAllText(Path.Combine(AppRoot, "MainWindow.xaml.cs"));
        var pageNames = Regex.Matches(mainWindowCode, @"typeof\((?<page>[A-Za-z0-9_]+Page)\)")
            .Select(match => match.Groups["page"].Value)
            .Distinct(StringComparer.Ordinal)
            .ToArray();

        Assert.NotEmpty(pageNames);
        foreach (var pageName in pageNames)
        {
            Assert.True(
                File.Exists(Path.Combine(AppRoot, "Views", pageName + ".xaml")),
                $"Navigation references {pageName}, but Views/{pageName}.xaml does not exist.");
        }
    }

    [Theory]
    [InlineData("DiagnosticsPage.xaml")]
    [InlineData("ExperimentsPage.xaml")]
    [InlineData("FileLockPage.xaml")]
    [InlineData("NetworkPage.xaml")]
    [InlineData("ReliabilityPage.xaml")]
    [InlineData("SettingsPage.xaml")]
    [InlineData("SystemChangesPage.xaml")]
    public void ResponsiveInvestigationPagesKeepCompactMediumAndWideStates(string fileName)
    {
        var document = XDocument.Load(Path.Combine(AppRoot, "Views", fileName));
        var stateNames = document.Descendants(Presentation + "VisualState")
            .Select(state => state.Attribute(Xaml + "Name")?.Value)
            .Where(name => name is not null)
            .ToHashSet(StringComparer.Ordinal);
        var breakpoints = document.Descendants(Presentation + "AdaptiveTrigger")
            .Select(trigger => trigger.Attribute("MinWindowWidth")?.Value)
            .Where(value => value is not null)
            .ToHashSet(StringComparer.Ordinal);

        Assert.Contains("Compact", stateNames);
        Assert.Contains("Medium", stateNames);
        Assert.Contains("Wide", stateNames);
        Assert.Contains("640", breakpoints);
        Assert.Contains("1008", breakpoints);
    }

    [Fact]
    public void ThemeDictionariesDefineEveryAtlasBrushInHighContrast()
    {
        var document = XDocument.Load(Path.Combine(AppRoot, "Themes", "DesignTokens.xaml"));
        var dictionaries = document.Descendants(Presentation + "ResourceDictionary")
            .Where(dictionary => dictionary.Attribute(Xaml + "Key") is not null)
            .ToDictionary(
                dictionary => dictionary.Attribute(Xaml + "Key")!.Value,
                dictionary => dictionary.Elements()
                    .Select(element => element.Attribute(Xaml + "Key")?.Value)
                    .Where(key => key?.StartsWith("Atlas", StringComparison.Ordinal) is true)
                    .Cast<string>()
                    .ToHashSet(StringComparer.Ordinal),
                StringComparer.Ordinal);

        var dark = dictionaries["Dark"];
        Assert.NotEmpty(dark);
        Assert.True(dark.SetEquals(dictionaries["Light"]), "Light and Dark Atlas brush keys differ.");
        Assert.True(dark.SetEquals(dictionaries["HighContrast"]),
            "High Contrast must define every Atlas brush used by the app.");
    }

    [Fact]
    public void ExperimentsExposeRealActionsResponsiveStatesAndEvidenceCaveat()
    {
        var xamlPath = Path.Combine(AppRoot, "Views", "ExperimentsPage.xaml");
        var xaml = File.ReadAllText(xamlPath);
        var code = File.ReadAllText(xamlPath + ".cs");

        Assert.Contains("Click=\"NewExperiment_Click\"", xaml, StringComparison.Ordinal);
        Assert.Contains("Click=\"Export_Click\"", xaml, StringComparison.Ordinal);
        Assert.Contains("SelectionChanged=\"ExperimentList_SelectionChanged\"", xaml, StringComparison.Ordinal);
        Assert.Contains("result.Caveat", code, StringComparison.Ordinal);
        Assert.Contains("ExperimentInsufficientData", code, StringComparison.Ordinal);
    }

    [Fact]
    public void PrivacyAlertDialogGuardsSelectionChangedDuringXamlInitialization()
    {
        var code = File.ReadAllText(Path.Combine(AppRoot, "Views", "PrivacyAlertEditDialog.xaml.cs"));

        Assert.Contains("if (ThresholdPanel is null)", code, StringComparison.Ordinal);
    }

    [Fact]
    public void RuleDialogGuardsSelectionChangedDuringXamlInitialization()
    {
        var code = File.ReadAllText(Path.Combine(AppRoot, "Views", "RuleEditDialog.xaml.cs"));

        Assert.Contains("if (CustomMaskPanel is null)", code, StringComparison.Ordinal);
        Assert.Contains("if (GpuThresholdPanel is null)", code, StringComparison.Ordinal);
    }

    [Theory]
    [MemberData(nameof(XamlFiles))]
    public void IconOnlyButtonsHaveAccessibleNames(string path)
    {
        var document = XDocument.Load(path);
        var failures = document.Descendants(Presentation + "Button")
            .Where(button => button.Attribute("Content") is null)
            .Where(button => !button.Descendants(Presentation + "TextBlock")
                .Any(text => text.Attribute("Text") is not null || !string.IsNullOrWhiteSpace(text.Value)))
            .Where(button => button.Attribute("AutomationProperties.Name") is null)
            .Select(button =>
            {
                var line = ((IXmlLineInfo)button).HasLineInfo() ? ((IXmlLineInfo)button).LineNumber : 0;
                return $"{button.Attribute(Xaml + "Name")?.Value ?? "unnamed Button"} at line {line}";
            })
            .ToArray();

        Assert.True(failures.Length == 0,
            $"Icon-only buttons require AutomationProperties.Name in {path}: {string.Join(", ", failures)}");
    }
}
