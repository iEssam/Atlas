using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Data;
using Microsoft.UI.Xaml.Media;

namespace Atlas.App.Converters;

/// <summary>
/// Maps a monitor color token (from <c>MonitorFormatter</c>'s state/result/degraded
/// tokens) to a theme brush. Deliberately <b>calm</b>: the strongest color used is
/// caution, never the alarming red reserved for genuine danger — a TCP socket in
/// TIME_WAIT, a task that returned a non-zero code, or a boot that was slower than
/// usual are all information, not alarms (task brief §1 tone). Kept out of the
/// view-models so no WinUI type leaks into the testable formatting layer.
///
/// <list type="bullet">
///   <item>"active" / "ok" → success (connected / succeeded)</item>
///   <item>"listen" → accent (informational: something is listening)</item>
///   <item>"transitional" / "attention" → caution (handshake/teardown, or a
///   non-zero result worth a glance)</item>
///   <item>"idle" / "none" / anything else → secondary text (neutral)</item>
/// </list>
/// </summary>
public sealed class MonitorTokenToBrushConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        var token = value as string;
        var key = token switch
        {
            "active" or "ok" => "SystemFillColorSuccessBrush",
            "listen" => "AccentFillColorDefaultBrush",
            "transitional" or "attention" => "SystemFillColorCautionBrush",
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
