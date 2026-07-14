using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Data;
using Microsoft.UI.Xaml.Media;

namespace Atlas.App.Converters;

/// <summary>
/// Maps a system-change category token (from
/// <c>R3Formatter.SystemChangeCategoryToken</c>) to a <b>calm</b> theme brush for
/// the change's leading glyph / dot. A change is information about what happened —
/// never a threat — so this palette is deliberately muted blue / green / neutral
/// and <b>never</b> touches the red critical brush. Kept out of the view-models so
/// no WinUI type leaks into the testable formatting layer.
/// </summary>
public sealed class ChangeCategoryToBrushConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        var token = value as string;
        var key = token switch
        {
            "install" => "SystemFillColorSuccessBrush",
            "startup" => "SystemFillColorSuccessBrush",
            "update" => "SystemFillColorAttentionBrush",
            "driver" => "SystemFillColorAttentionBrush",
            "service" => "AccentFillColorDefaultBrush",
            "task" => "AccentFillColorDefaultBrush",
            "remove" => "TextFillColorSecondaryBrush",
            "power" => "TextFillColorTertiaryBrush",
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
