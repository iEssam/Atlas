using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Data;
using Microsoft.UI.Xaml.Media;

namespace Atlas.App.Converters;

/// <summary>
/// Maps the shell's real capture-active flag to the status-dot brush: mineral
/// cyan when a live source (ring/stream) is connected, subdued amber when it is
/// not. Colours resolve from the app palette so they track the theme.
/// </summary>
public sealed class CaptureActiveToBrushConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        var active = value is bool b && b;
        var key = active ? "AtlasCyanBrush" : "AtlasAmberBrush";
        if (Application.Current.Resources.TryGetValue(key, out var brush) && brush is Brush br)
        {
            return br;
        }
        return new SolidColorBrush(active
            ? Microsoft.UI.ColorHelper.FromArgb(0xFF, 0x7F, 0xC6, 0xC0)
            : Microsoft.UI.ColorHelper.FromArgb(0xFF, 0xC3, 0xA0, 0x6A));
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language)
        => throw new NotSupportedException();
}
