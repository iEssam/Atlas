using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Data;
using Microsoft.UI.Xaml.Media;

namespace Atlas.App.Converters;

/// <summary>
/// Maps a crash-record caution token (from <c>R3Formatter.CrashCautionToken</c>)
/// to a theme brush for the record's leading glyph / dot: "caution" → the amber
/// caution brush, "neutral" → muted secondary text. The red <b>critical</b> brush
/// is deliberately unreachable here — a crash record is history and correlated
/// context to understand, not an alarm to raise (task brief; PRD §9.14). Kept out
/// of the view-models so no WinUI type leaks into the testable formatting layer.
/// </summary>
public sealed class CrashCautionToBrushConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        var token = value as string;
        var key = token switch
        {
            "caution" => "SystemFillColorCautionBrush",
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
