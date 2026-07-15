using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Data;
using Microsoft.UI.Xaml.Media;

namespace Atlas.App.Converters;

/// <summary>
/// Maps a Security-tab token to a calm theme brush. It covers two neutral
/// vocabularies produced by <c>SecurityFormatter</c>:
/// <list type="bullet">
/// <item>privilege state — "enabled" (an informational accent) and "available"
/// (muted secondary);</item>
/// <item>certificate validity — "ok" (muted secondary), "caution" (expiring soon
/// / not yet valid) and "expired", both the caution amber.</item>
/// </list>
///
/// <para>
/// The design constraint from the task brief: this is <b>expert data shown
/// factually</b>. A held privilege — enabled or not — is a normal part of a
/// process token, so neither state borrows an alarm color; and an expired or
/// soon-to-expire signing certificate tops out at the amber the app uses for
/// "worth a glance", <b>never</b> the red reserved for genuine danger.
/// </para>
/// </summary>
public sealed class SecurityTokenToBrushConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        var token = value as string;
        var key = token switch
        {
            "enabled" => "AccentTextFillColorPrimaryBrush",
            "caution" => "SystemFillColorCautionBrush",
            "expired" => "SystemFillColorCautionBrush",
            // "available", "ok", and anything unknown stay muted, not colored.
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
