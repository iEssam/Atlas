using Atlas.App.ViewModels;
using Atlas.App.Views;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace Atlas.App;

/// <summary>
/// The single application window: an evidence-console shell (grouped
/// NavigationView, top status bar, sidebar footer) hosting the page frame. The
/// shell status binds only to real device/connection state (<see cref="ShellViewModel"/>).
/// </summary>
public sealed partial class MainWindow : Window
{
    /// <summary>Shell status view model (device/session + real capture state).</summary>
    public ShellViewModel ViewModel { get; }

    public MainWindow()
    {
        ViewModel = new ShellViewModel(DispatcherQueue);
        InitializeComponent();

        Closed += (_, _) => ViewModel.Stop();

        ContentFrame.Navigate(typeof(OverviewPage));
    }

    private void Nav_SelectionChanged(NavigationView sender, NavigationViewSelectionChangedEventArgs args)
    {
        var type = (args.SelectedItemContainer as NavigationViewItem)?.Tag switch
        {
            "overview" => typeof(OverviewPage),
            "live" => typeof(LiveActivityPage),
            "timeline" => typeof(TimelinePage),
            "search" => typeof(SearchPage),
            "privacy" => typeof(PrivacyPage),
            "privacyalerts" => typeof(PrivacyAlertsPage),
            "startup" => typeof(StartupPage),
            "services" => typeof(ServicesPage),
            "network" => typeof(NetworkPage),
            "scheduledtasks" => typeof(ScheduledTasksPage),
            "sensors" => typeof(SensorsPage),
            "filelock" => typeof(FileLockPage),
            "rules" => typeof(RulesPage),
            "profiles" => typeof(ProfilesPage),
            "diagnostics" => typeof(DiagnosticsPage),
            "systemchanges" => typeof(SystemChangesPage),
            "reliability" => typeof(ReliabilityPage),
            "plugins" => typeof(PluginsPage),
            "settings" => typeof(SettingsPage),
            _ => null,
        };
        if (type is not null && ContentFrame.CurrentSourcePageType != type)
        {
            ContentFrame.Navigate(type);
        }
    }
}
