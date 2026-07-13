using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Data;

namespace Atlas.App.Converters;

/// <summary>
/// Maps <c>false</c> → <see cref="Visibility.Visible"/> and <c>true</c> →
/// <see cref="Visibility.Collapsed"/>. Used to show the results area only when
/// the history/search surface is <b>not</b> in its unavailable state.
/// </summary>
public sealed class InverseBoolToVisibilityConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        var flag = value is bool b && b;
        return flag ? Visibility.Collapsed : Visibility.Visible;
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language)
    {
        return value is Visibility v && v == Visibility.Collapsed;
    }
}
