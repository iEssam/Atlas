using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Data;
using Microsoft.UI.Xaml.Media;

namespace Atlas.App.Converters;

/// <summary>
/// Maps a signature trust token (from <c>R2Formatter.SignatureTrustToken</c> /
/// the module signed token) to a calm theme brush for the signature badge:
/// "trusted" → success, "signed" → accent (a confident blue), "caution"
/// (unsigned) → the caution amber, "unknown" → secondary text.
///
/// <para>
/// The design constraint from the task brief: an <b>unsigned</b> binary is
/// common and legitimate, so "caution" is the amber the app uses for "worth a
/// glance", <b>never</b> the alarming red reserved for genuine danger. Blank or
/// unknown signature states are muted, not colored, so the UI never implies a
/// process is suspicious because a field couldn't be read.
/// </para>
/// </summary>
public sealed class TrustTokenToBrushConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        var token = value as string;
        var key = token switch
        {
            "trusted" => "SystemFillColorSuccessBrush",
            "signed" => "AccentTextFillColorPrimaryBrush",
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
