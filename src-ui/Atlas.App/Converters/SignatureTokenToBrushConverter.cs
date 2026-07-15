using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Data;
using Microsoft.UI.Xaml.Media;

namespace Atlas.App.Converters;

/// <summary>
/// Maps a plugin signature token (from <c>PluginFormatter.SignatureColorToken</c>)
/// to a theme brush for the signature badge: "signed" → success (calm positive),
/// "unsigned" → caution (a caution, deliberately <b>not</b> the red danger
/// palette), "unknown" → secondary text (neutral). Kept out of the view-models so
/// no WinUI type leaks into the testable formatting layer. The framing is the
/// point: an unsigned plugin is something to weigh, never something to fear.
/// </summary>
public sealed class SignatureTokenToBrushConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        var token = value as string;
        var key = token switch
        {
            "signed" => "SystemFillColorSuccessBrush",
            "unsigned" => "SystemFillColorCautionBrush",
            _ => "TextFillColorSecondaryBrush",
        };
        if (Application.Current.Resources.TryGetValue(key, out var brush) && brush is Brush b)
        {
            return b;
        }
        return new SolidColorBrush(Microsoft.UI.Colors.Gray);
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language) =>
        throw new NotSupportedException();
}
