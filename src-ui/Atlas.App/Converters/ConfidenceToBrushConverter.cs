using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Data;
using Microsoft.UI.Xaml.Media;

namespace Atlas.App.Converters;

/// <summary>
/// Maps a confidence color token (from <c>M8Formatter.ConfidenceColorToken</c>) to
/// a <b>calm, epistemic</b> theme brush for the confidence badge. This is a
/// deliberate design constraint from the task brief: a low-confidence factor is
/// the engine being honest about weak evidence, <b>not</b> a danger, so it must
/// never be painted the alarming red the incident-severity scale reserves for
/// something genuinely critical.
///
/// <list type="bullet">
/// <item>"confirmed" → success green — this rung is a measured fact.</item>
/// <item>"high" → the accent color — a calm, confident blue, not a warning.</item>
/// <item>"medium" → primary text — neutral and readable.</item>
/// <item>"low"/"insufficient" → muted secondary/tertiary text — quiet, not red.</item>
/// </list>
/// </summary>
public sealed class ConfidenceToBrushConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        var token = value as string;
        var key = token switch
        {
            "confirmed" => "SystemFillColorSuccessBrush",
            "high" => "AccentTextFillColorPrimaryBrush",
            "medium" => "TextFillColorPrimaryBrush",
            "low" => "TextFillColorSecondaryBrush",
            "insufficient" => "TextFillColorTertiaryBrush",
            _ => "TextFillColorTertiaryBrush",
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
