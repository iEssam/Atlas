using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.ApplicationModel.DataTransfer;

namespace Atlas.App.Views;

/// <summary>
/// The Settings / Integrations page. Its one section documents the read-only MCP
/// integration (tech-stack §4.7, PRD §9.16): what it is (Atlas exposes grounded
/// query tools to the user's own AI client; Atlas hosts no model), that it is off
/// by default and lives in a separate <c>atlas-mcp</c> process the user registers
/// in their client, the exact read-only tools it exposes, and — prominently and
/// calmly — the honest boundary warning that tool results leave Atlas's security
/// boundary for the client's model provider (redaction applied by default).
///
/// <para>
/// This page is purely informational: Atlas does not launch or enable the MCP
/// server. There is deliberately no enable toggle that would do nothing — the
/// affordance is a copyable client-config snippet plus clear documentation, so the
/// UI never implies it controls something it doesn't (task brief §3).
/// </para>
/// </summary>
public sealed partial class SettingsPage : Page
{
    /// <summary>
    /// The example MCP-client configuration (Claude Desktop / generic MCP host
    /// format). The path is a placeholder the user adjusts to their install.
    /// </summary>
    private const string ConfigSnippet =
        "{\n" +
        "  \"mcpServers\": {\n" +
        "    \"system-atlas\": {\n" +
        "      \"command\": \"C:\\\\Program Files\\\\System Atlas\\\\atlas-mcp.exe\",\n" +
        "      \"args\": []\n" +
        "    }\n" +
        "  }\n" +
        "}";

    public SettingsPage()
    {
        InitializeComponent();
        ConfigBox.Text = ConfigSnippet;
    }

    private void CopyConfig_Click(object sender, RoutedEventArgs e)
    {
        var package = new DataPackage();
        package.SetText(ConfigSnippet);
        Clipboard.SetContent(package);
        CopyStatus.IsOpen = true;
    }
}
