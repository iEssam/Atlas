using System.Runtime.InteropServices;
using Atlas.App.Models;
using Atlas.App.ViewModels;
using Atlas.App.Views;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.Graphics;

namespace Atlas.App;

/// <summary>The single, unprivileged Evidence Atlas application window.</summary>
public sealed partial class MainWindow : Window
{
    private static readonly IReadOnlyDictionary<string, NavigationSection> Sections =
        new Dictionary<string, NavigationSection>(StringComparer.Ordinal)
        {
            ["activity"] = new("activity", "Activity",
                [new("Live Activity", typeof(LiveActivityPage)), new("Timeline", typeof(TimelinePage))]),
            ["performance"] = new("performance", "Performance",
                [new("Graphics", typeof(GpuPage)), new("Sensors", typeof(SensorsPage)), new("Network", typeof(NetworkPage))]),
            ["investigate"] = new("investigate", "Investigate",
                [new("Diagnostics", typeof(DiagnosticsPage)), new("System Changes", typeof(SystemChangesPage)), new("Reliability", typeof(ReliabilityPage)), new("File Locks", typeof(FileLockPage))]),
            ["system"] = new("system", "System",
                [new("Startup", typeof(StartupPage)), new("Services", typeof(ServicesPage)), new("Scheduled Tasks", typeof(ScheduledTasksPage))]),
            ["automation"] = new("automation", "Automation",
                [new("Rules", typeof(RulesPage)), new("Profiles", typeof(ProfilesPage))]),
            ["privacy"] = new("privacy", "Privacy",
                [new("Activity", typeof(PrivacyPage)), new("Alerts", typeof(PrivacyAlertsPage))]),
        };

    [DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(IntPtr windowHandle);

    public ShellViewModel ViewModel { get; }

    public MainWindow()
    {
        ViewModel = new ShellViewModel(DispatcherQueue);
        InitializeComponent();
        ExtendsContentIntoTitleBar = true;
        SetTitleBar(TitleBarDragRegion);
        SystemBackdrop = new MicaBackdrop();
        ApplyThemePreference(App.Preferences.Current.Theme);
        ResizeForFirstRun();
        Closed += (_, _) => ViewModel.Stop();
        ContentFrame.Navigate(typeof(OverviewPage));
    }

    public void ApplyThemePreference(ThemePreference preference)
    {
        Root.RequestedTheme = preference switch
        {
            ThemePreference.Light => ElementTheme.Light,
            ThemePreference.Dark => ElementTheme.Dark,
            _ => ElementTheme.Default,
        };
    }

    public void NavigateToEvidence(string kind)
    {
        var pageType = kind switch
        {
            "Incident" => typeof(DiagnosticsPage),
            "Privacy" => typeof(PrivacyPage),
            "Change" => typeof(SystemChangesPage),
            _ => typeof(TimelinePage),
        };
        ContentFrame.Navigate(pageType);
    }

    public void NavigateToInsightDestination(string destination)
    {
        var pageType = destination switch
        {
            "activity" => typeof(LiveActivityPage),
            "graphics" => typeof(GpuPage),
            _ => typeof(TimelinePage),
        };
        ContentFrame.Navigate(pageType);
    }

    private void ResizeForFirstRun()
    {
        var handle = WinRT.Interop.WindowNative.GetWindowHandle(this);
        var scale = Math.Max(1.0, GetDpiForWindow(handle) / 96.0);
        AppWindow.Resize(new SizeInt32((int)Math.Round(1280 * scale), (int)Math.Round(820 * scale)));
    }

    private void Nav_SelectionChanged(NavigationView sender, NavigationViewSelectionChangedEventArgs args)
    {
        var tag = (args.SelectedItemContainer as NavigationViewItem)?.Tag as string;
        if (tag is null)
        {
            return;
        }

        if (Sections.TryGetValue(tag, out var section))
        {
            ContentFrame.Navigate(typeof(SectionHostPage), section);
            return;
        }

        var pageType = tag switch
        {
            "overview" => typeof(OverviewPage),
            "plugins" => typeof(PluginsPage),
            "settings" => typeof(SettingsPage),
            _ => null,
        };
        if (pageType is not null && ContentFrame.CurrentSourcePageType != pageType)
        {
            ContentFrame.Navigate(pageType);
        }
    }

    private void GlobalSearchBox_QuerySubmitted(AutoSuggestBox sender, AutoSuggestBoxQuerySubmittedEventArgs args)
    {
        var query = (args.QueryText ?? sender.Text).Trim();
        if (query.Length == 0)
        {
            return;
        }

        ContentFrame.Navigate(typeof(SearchPage), query);
        sender.Text = string.Empty;
    }

    private void SearchAccelerator_Invoked(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        GlobalSearchBox.Focus(FocusState.Keyboard);
        args.Handled = true;
    }

    private void BackAccelerator_Invoked(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        if (ContentFrame.CanGoBack)
        {
            ContentFrame.GoBack();
            args.Handled = true;
        }
    }
}
