using System.Text.Json;
using Atlas.App.Models;

namespace Atlas.App.Services;

public interface IUiPreferencesStore
{
    UiPreferences Current { get; }

    Task SaveAsync(UiPreferences preferences, CancellationToken cancellationToken = default);
}

/// <summary>
/// Stores UI-only preferences under the current user's local application-data
/// directory. Writes are replaced atomically and never include search history
/// or system evidence.
/// </summary>
public sealed class UiPreferencesStore : IUiPreferencesStore
{
    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        WriteIndented = true,
    };

    private readonly string _path;
    private readonly SemaphoreSlim _writeGate = new(1, 1);

    public UiPreferences Current { get; private set; }

    public UiPreferencesStore()
    {
        var directory = Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
            "SystemAtlas");
        _path = Path.Combine(directory, "ui-settings.json");
        Current = Load(_path);
    }

    public async Task SaveAsync(
        UiPreferences preferences,
        CancellationToken cancellationToken = default)
    {
        Current = preferences;
        await _writeGate.WaitAsync(cancellationToken);
        try
        {
            var directory = Path.GetDirectoryName(_path)!;
            Directory.CreateDirectory(directory);

            var temporaryPath = _path + ".tmp";
            await using (var stream = new FileStream(
                temporaryPath,
                FileMode.Create,
                FileAccess.Write,
                FileShare.None,
                4096,
                FileOptions.Asynchronous | FileOptions.WriteThrough))
            {
                await JsonSerializer.SerializeAsync(
                    stream,
                    Current,
                    JsonOptions,
                    cancellationToken);
                await stream.FlushAsync(cancellationToken);
            }

            File.Move(temporaryPath, _path, overwrite: true);
        }
        finally
        {
            _writeGate.Release();
        }
    }

    private static UiPreferences Load(string path)
    {
        try
        {
            if (!File.Exists(path))
            {
                return new UiPreferences();
            }

            var json = File.ReadAllText(path);
            return JsonSerializer.Deserialize<UiPreferences>(json, JsonOptions)
                ?? new UiPreferences();
        }
        catch (JsonException)
        {
            return new UiPreferences();
        }
        catch (IOException)
        {
            return new UiPreferences();
        }
        catch (UnauthorizedAccessException)
        {
            return new UiPreferences();
        }
    }
}
