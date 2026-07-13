using System;
using System.Collections.ObjectModel;
using System.Threading;
using System.Threading.Tasks;
using Atlas.IpcClient;
using Atlas.V0;
using CommunityToolkit.Mvvm.ComponentModel;
using Microsoft.UI.Dispatching;

namespace Atlas.App.ViewModels;

/// <summary>
/// Drives the Search page (M6): a single query fans out to the service's Search
/// RPC and the hits are grouped by kind (processes / events / bookmarks) with a
/// type icon. Empty and "unavailable" (server too old) states are first-class.
/// </summary>
public sealed partial class SearchViewModel : ObservableObject
{
    private readonly DispatcherQueue _dispatcher;
    private readonly string? _who;
    private CancellationTokenSource? _cts;

    [ObservableProperty] private string _query = string.Empty;
    [ObservableProperty] private bool _isSearching;
    [ObservableProperty] private bool _isUnavailable;
    [ObservableProperty] private bool _hasSearched;
    [ObservableProperty] private string _statusText = string.Empty;

    public ObservableCollection<SearchResultGroup> Groups { get; } = new();

    public SearchViewModel(DispatcherQueue dispatcher, string? who = null)
    {
        _dispatcher = dispatcher;
        _who = who;
    }

    /// <summary>Runs the search for the current <see cref="Query"/>.</summary>
    public async Task SearchAsync()
    {
        var q = Query?.Trim() ?? string.Empty;
        if (q.Length == 0)
        {
            Groups.Clear();
            HasSearched = false;
            StatusText = string.Empty;
            IsUnavailable = false;
            return;
        }

        _cts?.Cancel();
        var cts = new CancellationTokenSource();
        _cts = cts;
        var ct = cts.Token;

        IsSearching = true;
        IsUnavailable = false;
        HasSearched = true;
        StatusText = "Searching…";

        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.SearchAsync(q, limit: 100, cancellationToken: ct)
                .ConfigureAwait(false);

            if (ct.IsCancellationRequested)
            {
                return;
            }

            if (!outcome.Supported)
            {
                Post(() =>
                {
                    Groups.Clear();
                    IsUnavailable = true;
                    StatusText = "Search unavailable — the service is too old.";
                    IsSearching = false;
                });
                return;
            }

            var processes = new SearchResultGroup("Processes", "");
            var events = new SearchResultGroup("Events", "");
            var bookmarks = new SearchResultGroup("Bookmarks", "");

            foreach (var hit in outcome.Value.Hits)
            {
                switch (hit.EntityCase)
                {
                    case SearchHit.EntityOneofCase.Process:
                        processes.Items.Add(new SearchResultItem(
                            processes.Icon,
                            hit.Process.ImageName,
                            $"pid {hit.Process.Pid} • {(hit.Process.Live ? "running" : "exited")}"));
                        break;
                    case SearchHit.EntityOneofCase.Event:
                        events.Items.Add(new SearchResultItem(
                            events.Icon,
                            HistoryFormatter.EventLine(hit.Event),
                            FormatTs(hit.Event.TsMs)));
                        break;
                    case SearchHit.EntityOneofCase.Bookmark:
                        bookmarks.Items.Add(new SearchResultItem(
                            bookmarks.Icon,
                            string.IsNullOrEmpty(hit.Bookmark.Label) ? "Bookmark" : hit.Bookmark.Label,
                            FormatTs(hit.Bookmark.TsMs)));
                        break;
                }
            }

            Post(() =>
            {
                Groups.Clear();
                foreach (var g in new[] { processes, events, bookmarks })
                {
                    if (g.Items.Count > 0)
                    {
                        Groups.Add(g);
                    }
                }
                int total = processes.Items.Count + events.Items.Count + bookmarks.Items.Count;
                StatusText = total == 0 ? "No matches." : $"{total} result{(total == 1 ? "" : "s")}.";
                IsSearching = false;
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
                Groups.Clear();
                IsUnavailable = true;
                StatusText = $"Could not reach the service: {ex.Message}";
                IsSearching = false;
            });
        }
    }

    public void Stop() => _cts?.Cancel();

    private static string FormatTs(long tsMs) =>
        DateTimeOffset.FromUnixTimeMilliseconds(tsMs).LocalDateTime.ToString("g");

    private void Post(Action action) => _dispatcher.TryEnqueue(() => action());
}

/// <summary>A grouped set of search hits of one kind, with a type glyph.</summary>
public sealed class SearchResultGroup
{
    public string Title { get; }
    public string Icon { get; }
    public ObservableCollection<SearchResultItem> Items { get; } = new();

    public SearchResultGroup(string title, string icon)
    {
        Title = title;
        Icon = icon;
    }
}

/// <summary>One search hit line: a type glyph, a primary label, and a subtitle.</summary>
public sealed class SearchResultItem
{
    public string Icon { get; }
    public string Primary { get; }
    public string Secondary { get; }

    public SearchResultItem(string icon, string primary, string secondary)
    {
        Icon = icon;
        Primary = primary;
        Secondary = secondary;
    }
}
