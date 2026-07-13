using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Data;

namespace Atlas.App.Converters;

/// <summary>
/// Maps an <see cref="int"/> count to visibility inverted: <c>0</c> →
/// <see cref="Visibility.Visible"/> (show the empty-state text), any positive
/// count → <see cref="Visibility.Collapsed"/>. Used for per-group "nothing here"
/// placeholders where the count comes straight from a collection.
/// </summary>
public sealed class CountToInverseVisibilityConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        var count = value is int i ? i : 0;
        return count == 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language) =>
        throw new NotSupportedException();
}
