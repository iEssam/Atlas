using Atlas.App.Views;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace Atlas.App;

/// <summary>
/// The single application window: a NavigationView shell with one functional
/// page (Live Activity). Mica backdrop applied when available.
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

        ContentFrame.Navigate(typeof(LiveActivityPage));
    }

    private void Nav_SelectionChanged(NavigationView sender, NavigationViewSelectionChangedEventArgs args)
    {
        if (args.SelectedItemContainer is NavigationViewItem { Tag: "live" })
        {
            ContentFrame.Navigate(typeof(LiveActivityPage));
        }
    }
}
