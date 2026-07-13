using Microsoft.UI.Xaml;

namespace Atlas.App;

/// <summary>
/// Application entry point. Unpackaged WinUI 3 host: creates the single main
/// window on launch.
/// </summary>
public partial class App : Application
{
    private Window? _window;

    /// <summary>
    /// The single main window, exposed so unpackaged pickers/dialogs (e.g. the
    /// M8 report <c>FileSavePicker</c>) can obtain an owner HWND via
    /// <c>WinRT.Interop.WindowNative.GetWindowHandle</c>. Null before launch.
    /// </summary>
    public static Window? MainWindow { get; private set; }

    public App()
    {
        InitializeComponent();
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        _window = new MainWindow();
        MainWindow = _window;
        _window.Activate();
    }
}
