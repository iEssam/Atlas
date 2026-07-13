using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Data;
using Microsoft.UI.Xaml.Media;

namespace Atlas.App.Converters;

/// <summary>
/// Maps an incident-severity color token (from <c>M8Formatter.SeverityColorToken</c>)
/// to a theme brush for the severity dot/label. Unlike the confidence scale, this
/// is the one place the <b>danger</b> palette belongs: a warning is caution amber
/// and a critical incident is the critical red — an accurate signal about the
/// system, not about the engine's certainty.
/// </summary>
public sealed class SeverityToBrushConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        var token = value as string;
        var key = token switch
        {
            "critical" => "SystemFillColorCriticalBrush",
            "warning" => "SystemFillColorCautionBrush",
            "info" => "SystemFillColorSuccessBrush",
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
