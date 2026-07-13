using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Data;
using Microsoft.UI.Xaml.Media;

namespace Atlas.App.Converters;

/// <summary>
/// Maps a simulated target's <c>Blocked</c> flag to a row background brush for the
/// simulation preview. A protected-critical target gets the calm caution
/// <em>background</em> tint (a soft, low-saturation wash) so it reads as clearly
/// distinct from the actionable rows — but never as an error. The framing is
/// "Atlas is protecting this", not "something went wrong": genuine-danger red is
/// deliberately not used here (task brief — blocked/protected targets must be
/// clearly but calmly surfaced). Non-blocked rows stay transparent.
/// </summary>
public sealed class BlockedToBrushConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        if (value is bool blocked && blocked)
        {
            if (Application.Current.Resources.TryGetValue(
                    "SystemFillColorCautionBackgroundBrush", out var brush) && brush is Brush b)
            {
                return b;
            }
            return new SolidColorBrush(Microsoft.UI.Colors.Transparent);
        }
        return new SolidColorBrush(Microsoft.UI.Colors.Transparent);
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language) =>
        throw new NotSupportedException();
}
