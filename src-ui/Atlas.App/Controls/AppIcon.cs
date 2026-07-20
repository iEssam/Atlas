using System.Diagnostics;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media.Imaging;
using Windows.Storage;
using Windows.Storage.FileProperties;

namespace Atlas.App.Controls;

/// <summary>
/// Displays the real icon embedded in an executable. Path resolution runs off the UI thread;
/// Windows' thumbnail provider supplies and caches the decoded executable icon.
/// </summary>
public sealed class AppIcon : Grid
{
    private readonly Image _image;
    private readonly FontIcon _fallback;
    private long _loadVersion;

    public AppIcon()
    {
        Width = 20;
        Height = 20;
        VerticalAlignment = VerticalAlignment.Center;

        _fallback = new FontIcon
        {
            Glyph = "\uE7C3",
            FontSize = 15,
            Opacity = 0.62,
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center
        };
        _image = new Image
        {
            Stretch = Microsoft.UI.Xaml.Media.Stretch.Uniform,
            Visibility = Visibility.Collapsed
        };
        Children.Add(_fallback);
        Children.Add(_image);
        Loaded += (_, _) => BeginLoad();
        Unloaded += (_, _) => Interlocked.Increment(ref _loadVersion);
    }

    public static readonly DependencyProperty ExecutablePathProperty = DependencyProperty.Register(
        nameof(ExecutablePath), typeof(string), typeof(AppIcon),
        new PropertyMetadata(string.Empty, OnIdentityChanged));

    public static readonly DependencyProperty CommandProperty = DependencyProperty.Register(
        nameof(Command), typeof(string), typeof(AppIcon),
        new PropertyMetadata(string.Empty, OnIdentityChanged));

    public static readonly DependencyProperty ProcessIdProperty = DependencyProperty.Register(
        nameof(ProcessId), typeof(uint), typeof(AppIcon),
        new PropertyMetadata(0u, OnIdentityChanged));

    public string ExecutablePath
    {
        get => (string)GetValue(ExecutablePathProperty);
        set => SetValue(ExecutablePathProperty, value);
    }

    public string Command
    {
        get => (string)GetValue(CommandProperty);
        set => SetValue(CommandProperty, value);
    }

    public uint ProcessId
    {
        get => (uint)GetValue(ProcessIdProperty);
        set => SetValue(ProcessIdProperty, value);
    }

    private static void OnIdentityChanged(DependencyObject sender, DependencyPropertyChangedEventArgs args)
    {
        if (sender is AppIcon icon && icon.IsLoaded)
            icon.BeginLoad();
    }

    private async void BeginLoad()
    {
        long version = Interlocked.Increment(ref _loadVersion);
        _image.Source = null;
        _image.Visibility = Visibility.Collapsed;
        _fallback.Visibility = Visibility.Visible;

        string executablePath = ExecutablePath;
        string command = Command;
        uint processId = ProcessId;
        string? path = await Task.Run(() => ResolvePath(executablePath, command, processId));
        if (version != _loadVersion || string.IsNullOrWhiteSpace(path))
            return;

        BitmapImage? source = await LoadThumbnailAsync(path);
        if (version != _loadVersion)
            return;

        if (source is null)
            return;

        _image.Source = source;
        _image.Visibility = Visibility.Visible;
        _fallback.Visibility = Visibility.Collapsed;
    }

    private static string? ResolvePath(string executablePath, string command, uint processId)
    {
        string? direct = NormalizeCandidate(executablePath);
        if (direct is not null)
            return direct;

        string? commandPath = NormalizeCandidate(FirstCommandToken(command));
        if (commandPath is not null)
            return commandPath;

        if (processId == 0)
            return null;

        try
        {
            using Process process = Process.GetProcessById(checked((int)processId));
            return NormalizeCandidate(process.MainModule?.FileName);
        }
        catch (Exception ex) when (ex is ArgumentException or InvalidOperationException or
                                   System.ComponentModel.Win32Exception or NotSupportedException)
        {
            return null;
        }
    }

    private static string? NormalizeCandidate(string? candidate)
    {
        if (string.IsNullOrWhiteSpace(candidate))
            return null;

        string expanded = Environment.ExpandEnvironmentVariables(candidate.Trim().Trim('"'));
        if (expanded.Length > 3 && char.IsAsciiLetter(expanded[0]) && expanded[1] == ':' && expanded[2] == '#')
            expanded = expanded.Replace('#', Path.DirectorySeparatorChar);
        try
        {
            return File.Exists(expanded) ? Path.GetFullPath(expanded) : null;
        }
        catch (Exception ex) when (ex is ArgumentException or NotSupportedException or PathTooLongException)
        {
            return null;
        }
    }

    private static string FirstCommandToken(string? command)
    {
        if (string.IsNullOrWhiteSpace(command))
            return string.Empty;
        string value = command.Trim();
        if (value[0] == '"')
        {
            int close = value.IndexOf('"', 1);
            return close > 1 ? value[1..close] : value.Trim('"');
        }
        int separator = value.IndexOfAny([' ', '\t']);
        return separator < 0 ? value : value[..separator];
    }

    private static async Task<BitmapImage?> LoadThumbnailAsync(string path)
    {
        try
        {
            StorageFile file = await StorageFile.GetFileFromPathAsync(path);
            using StorageItemThumbnail thumbnail = await file.GetThumbnailAsync(
                ThumbnailMode.SingleItem, 32, ThumbnailOptions.UseCurrentScale);
            if (thumbnail.Type != ThumbnailType.Icon && thumbnail.Size == 0)
                return null;
            var source = new BitmapImage();
            await source.SetSourceAsync(thumbnail);
            return source;
        }
        catch (Exception ex) when (ex is ArgumentException or IOException or UnauthorizedAccessException or
                                   System.Runtime.InteropServices.COMException)
        {
            return null;
        }
    }
}
