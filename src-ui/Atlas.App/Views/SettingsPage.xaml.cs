using System;
using Atlas.App.Models;
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
        "      \"command\": \"C:\\\\Program Files\\\\Atlas\\\\atlas-mcp.exe\",\n" +
        "      \"args\": []\n" +
        "    }\n" +
        "  }\n" +
        "}";

    private readonly string? _who;
    private bool _initializing = true;

    public SettingsPage()
    {
        var who = Environment.GetEnvironmentVariable("ATLAS_PIPE");
        _who = string.IsNullOrEmpty(who) ? null : who;

        InitializeComponent();
        ConfigBox.Text = ConfigSnippet;
        ThemePicker.SelectedIndex = (int)App.Preferences.Current.Theme;
        DetailLevelPicker.SelectedIndex = (int)App.Preferences.Current.DetailLevel;
        _initializing = false;
    }

    private async void CreateBundle_Click(object sender, RoutedEventArgs e)
    {
        var dialog = new SupportBundleDialog(_who)
        {
            XamlRoot = XamlRoot,
        };
        await dialog.ShowAsync();
    }

    private void CopyConfig_Click(object sender, RoutedEventArgs e)
    {
        var package = new DataPackage();
        package.SetText(ConfigSnippet);
        Clipboard.SetContent(package);
        CopyStatus.IsOpen = true;
    }

    private async void ThemePicker_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_initializing || ThemePicker.SelectedItem is not ComboBoxItem { Tag: string tag }
            || !Enum.TryParse<ThemePreference>(tag, out var theme))
        {
            return;
        }

        var preferences = App.Preferences.Current;
        preferences.Theme = theme;
        if (App.MainWindow is MainWindow window)
        {
            window.ApplyThemePreference(theme);
        }
        await PersistPreferencesAsync(preferences);
    }

    private async void DetailLevelPicker_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_initializing || DetailLevelPicker.SelectedItem is not ComboBoxItem { Tag: string tag }
            || !Enum.TryParse<DetailLevel>(tag, out var detailLevel))
        {
            return;
        }

        var preferences = App.Preferences.Current;
        preferences.DetailLevel = detailLevel;
        await PersistPreferencesAsync(preferences);
    }

    private async Task PersistPreferencesAsync(UiPreferences preferences)
    {
        try
        {
            await App.Preferences.SaveAsync(preferences);
            PreferencesStatus.IsOpen = false;
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            PreferencesStatus.Severity = InfoBarSeverity.Error;
            PreferencesStatus.Title = "Could not save preferences";
            PreferencesStatus.Message = ex.Message;
            PreferencesStatus.IsOpen = true;
        }
    }
}
