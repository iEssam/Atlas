using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Data;

namespace Atlas.App.Converters;

/// <summary>
/// Maps <c>true</c> → <see cref="Visibility.Visible"/> and <c>false</c> →
/// <see cref="Visibility.Collapsed"/>. Used to reveal the dynamic-protection
/// config controls only while the master toggle is on.
/// </summary>
public sealed class BoolToVisibilityConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        var flag = value is bool b && b;
        return flag ? Visibility.Visible : Visibility.Collapsed;
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language)
    {
        return value is Visibility v && v == Visibility.Visible;
    }
}
