using Microsoft.UI.Xaml;

namespace Atlas.App;

/// <summary>
/// Application entry point. Unpackaged WinUI 3 host: creates the single main
/// window on launch.
/// </summary>
public partial class App : Application
{
    private Window? _window;

    public App()
    {
        InitializeComponent();
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        _window = new MainWindow();
        _window.Activate();
    }
}
