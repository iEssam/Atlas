using System;
using System.Collections.ObjectModel;
using System.Globalization;
using System.Threading;
using System.Threading.Tasks;
using Atlas.IpcClient;
using CommunityToolkit.Mvvm.ComponentModel;
using Microsoft.UI.Dispatching;

namespace Atlas.App.ViewModels;

/// <summary>
/// Drives the File-Lock Search page — "find what is using this file" (R2, PRD
/// §9.5). The user enters (or picks) a path; FindResourceOwners returns the
/// processes holding it open (Restart Manager first). Read-only this milestone —
/// the future "close / release" safe-action is present only as a disabled,
/// tooltip-explained hint (task brief §3).
///
/// <para>
/// The three outcomes are kept <b>distinct</b> so the answer is never ambiguous:
/// <c>available = false</c> (path not found / access denied — shown with the
/// service's reason), an empty owner list ("no process is holding this file" —
/// a clean, positive result), and a populated list. Against a service too old to
/// serve the RPC it shows a calm "unavailable" state instead of crashing.
/// </para>
/// </summary>
public sealed partial class FileLockViewModel : ObservableObject
{
    private static readonly CultureInfo Inv = CultureInfo.InvariantCulture;

    private readonly DispatcherQueue _dispatcher;
    private readonly string? _who;
    private CancellationTokenSource? _cts;

    [ObservableProperty] private string _pathInput = string.Empty;

    [ObservableProperty] private bool _isSearching;
    [ObservableProperty] private bool _hasSearched;
    [ObservableProperty] private bool _isUnavailable;
    [ObservableProperty] private string _unavailableText = string.Empty;
    [ObservableProperty] private bool _isEmpty;
    [ObservableProperty] private string _emptyText = string.Empty;
    [ObservableProperty] private string _statusText = string.Empty;

    /// <summary>True once a search returns at least one owner, so the read-only
    /// safe-release hint appears next to the results.</summary>
    [ObservableProperty] private bool _showReleaseHint;

    public ObservableCollection<ResourceOwnerItem> Owners { get; } = new();

    public FileLockViewModel(DispatcherQueue dispatcher, string? who = null)
    {
        _dispatcher = dispatcher;
        _who = who;
    }

    /// <summary>Runs the lookup for the current <see cref="PathInput"/>.</summary>
    public async Task SearchAsync()
    {
        var path = PathInput?.Trim() ?? string.Empty;
        if (path.Length == 0)
        {
            StatusText = "Enter a file path to search.";
            return;
        }

        _cts?.Cancel();
        var cts = new CancellationTokenSource();
        _cts = cts;
        var ct = cts.Token;

        IsSearching = true;
        IsUnavailable = false;
        IsEmpty = false;
        ShowReleaseHint = false;
        StatusText = "Searching…";

        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.FindResourceOwnersAsync(path, ct).ConfigureAwait(false);

            if (ct.IsCancellationRequested)
            {
                return;
            }

            Post(() =>
            {
                IsSearching = false;
                Owners.Clear();
                HasSearched = true;

                if (!outcome.Supported)
                {
                    IsUnavailable = true;
                    UnavailableText =
                        "This search needs a newer Atlas — the connected service is too old to look up file owners.";
                    StatusText = string.Empty;
                    return;
                }

                var reply = outcome.Value;
                if (!reply.Available)
                {
                    IsUnavailable = true;
                    UnavailableText = R2Formatter.UnavailableReason(
                        reply.UnavailableReason,
                        "That path couldn't be checked — it may not exist or may be inaccessible.");
                    StatusText = string.Empty;
                    return;
                }

                foreach (var o in reply.Owners)
                {
                    Owners.Add(new ResourceOwnerItem(
                        o.Pid,
                        string.IsNullOrWhiteSpace(o.ImageName) ? $"pid {o.Pid}" : o.ImageName,
                        R2Formatter.OrDash(o.ImagePath),
                        string.IsNullOrWhiteSpace(o.Description) ? "—" : o.Description,
                        o.IsService));
                }

                if (Owners.Count == 0)
                {
                    IsEmpty = true;
                    EmptyText = "No process is holding this file — it should be free to move, rename, or delete.";
                    StatusText = string.Empty;
                }
                else
                {
                    ShowReleaseHint = true;
                    StatusText = string.Format(
                        Inv,
                        "{0} process{1} using this file.",
                        Owners.Count,
                        Owners.Count == 1 ? "" : "es");
                }
            });
        }
        catch (OperationCanceledException)
        {
            // Superseded by a newer search.
        }
        catch (Exception ex)
        {
            Post(() =>
            {
                IsSearching = false;
                Owners.Clear();
                HasSearched = true;
                IsUnavailable = true;
                UnavailableText = $"Could not reach the service: {ex.Message}";
                StatusText = string.Empty;
            });
        }
    }

    public void Stop() => _cts?.Cancel();

    private void Post(Action action) => _dispatcher.TryEnqueue(() => action());
}

/// <summary>One owning process of a file, pre-formatted for the list.</summary>
public sealed class ResourceOwnerItem
{
    public uint Pid { get; }
    public string ImageName { get; }
    public string ImagePath { get; }
    public string Description { get; }
    public bool IsService { get; }

    public string PidText => $"pid {Pid}";

    /// <summary>Kind pill caption: "Service" vs "App".</summary>
    public string KindText => IsService ? "Service" : "App";

    public ResourceOwnerItem(
        uint pid, string imageName, string imagePath, string description, bool isService)
    {
        Pid = pid;
        ImageName = imageName;
        ImagePath = imagePath;
        Description = description;
        IsService = isService;
    }
}
