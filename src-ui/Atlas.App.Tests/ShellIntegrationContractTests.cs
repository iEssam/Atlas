using System.Xml.Linq;
using Xunit;

namespace Atlas.App.Tests;

public sealed class ShellIntegrationContractTests
{
    private const string CommandClsid = "2C9E70C5-5C34-4D34-984A-5956B8D0E11D";

    private static string RepositoryRoot
    {
        get
        {
            var directory = new DirectoryInfo(AppContext.BaseDirectory);
            while (directory is not null)
            {
                if (Directory.Exists(Path.Combine(directory.FullName, "shell-ext"))
                    && File.Exists(Path.Combine(directory.FullName, "Cargo.toml"))) return directory.FullName;
                directory = directory.Parent;
            }
            throw new DirectoryNotFoundException("Could not locate repository root.");
        }
    }

    [Fact]
    public void SparseManifestRegistersOneGenericFileCommandWithMatchingComClass()
    {
        var manifestPath = Path.Combine(RepositoryRoot, "shell-ext", "Package", "AppxManifest.xml");
        var document = XDocument.Load(manifestPath);
        var elements = document.Descendants().ToArray();
        var item = Assert.Single(elements, e => e.Name.LocalName == "ItemType");
        Assert.Equal("*", item.Attribute("Type")?.Value);
        var verb = Assert.Single(item.Elements(), e => e.Name.LocalName == "Verb");
        Assert.Equal(CommandClsid, verb.Attribute("Clsid")?.Value, ignoreCase: true);
        var comClass = Assert.Single(elements, e => e.Name.LocalName == "Class");
        Assert.Equal(CommandClsid, comClass.Attribute("Id")?.Value, ignoreCase: true);
        Assert.Equal("SystemAtlas.ShellExtension.dll", comClass.Attribute("Path")?.Value);
    }

    [Fact]
    public void ExplorerActivationIsRoutedIntoTheExistingFileLockSearch()
    {
        var app = File.ReadAllText(Path.Combine(RepositoryRoot, "src-ui", "Atlas.App", "App.xaml.cs"));
        var window = File.ReadAllText(Path.Combine(RepositoryRoot, "src-ui", "Atlas.App", "MainWindow.xaml.cs"));
        var page = File.ReadAllText(Path.Combine(RepositoryRoot, "src-ui", "Atlas.App", "Views", "FileLockPage.xaml.cs"));
        Assert.Contains("Environment.GetCommandLineArgs().Skip(1)", app, StringComparison.Ordinal);
        Assert.Contains("Navigate(typeof(FileLockPage), activation.FilePath)", window, StringComparison.Ordinal);
        Assert.Contains("ViewModel.PathInput = path", page, StringComparison.Ordinal);
        Assert.Contains("ViewModel.SearchAsync()", page, StringComparison.Ordinal);
    }
}
