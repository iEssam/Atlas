using Microsoft.UI.Xaml;

namespace Atlas.App.Views;

/// <summary>
/// A standalone, resizable Process Inspector window (R2, PRD §9.4). It is a thin
/// host: the real content lives in <see cref="InspectorView"/> (a UserControl, so
/// its <c>x:Bind</c> converters resolve). A separate window — rather than a modal
/// dialog — matches the "inspector" idiom: several processes can be inspected side
/// by side and the tables get room to breathe.
/// </summary>
public sealed partial class InspectorWindow : Window
{
    private readonly InspectorView _view;

    public InspectorWindow(string? who, uint pid, long createTime100ns, string imageName)
    {
        InitializeComponent();

        _view = new InspectorView(who, pid, createTime100ns, imageName);
        Root.Children.Add(_view);

        Title = _view.ViewModel.Title;

        try
        {
            AppWindow.Resize(new Windows.Graphics.SizeInt32(1040, 720));
        }
        catch
        {
            // Sizing is best-effort; ignore on hosts that don't support it.
        }
    }
}
