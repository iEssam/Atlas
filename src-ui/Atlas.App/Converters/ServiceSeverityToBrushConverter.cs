using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Data;
using Microsoft.UI.Xaml.Media;

namespace Atlas.App.Converters;

/// <summary>
/// Maps a service-state severity token (from
/// <c>M7Formatter.ServiceStateSeverity</c>) to a theme brush for the state dot:
/// "running" → success, "transitional" → caution, "stopped"/"unknown" →
/// secondary text. Kept out of the view-models so no WinUI type leaks into the
/// testable formatting layer.
/// </summary>
public sealed class ServiceSeverityToBrushConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        var token = value as string;
        var key = token switch
        {
            "running" => "SystemFillColorSuccessBrush",
            "transitional" => "SystemFillColorCautionBrush",
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
