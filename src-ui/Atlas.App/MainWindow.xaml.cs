using Atlas.App.Views;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace Atlas.App;

/// <summary>
/// The single application window: a NavigationView shell with Overview and Live
/// Activity pages. Mica backdrop applied when available.
/// </summary>
public sealed partial class MainWindow : Window
{
    public MainWindow()
    {
        InitializeComponent();

        // Mica backdrop — trivially available in WinAppSDK 1.6; the setter is a
        // no-op fallback on unsupported OSes.
        try
        {
            SystemBackdrop = new MicaBackdrop();
        }
        catch
        {
            // Leave default backdrop.
        }

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
            "startup" => typeof(StartupPage),
            "services" => typeof(ServicesPage),
            "diagnostics" => typeof(DiagnosticsPage),
            _ => null,
        };
        if (type is not null && ContentFrame.CurrentSourcePageType != type)
        {
            ContentFrame.Navigate(type);
        }
    }
}
