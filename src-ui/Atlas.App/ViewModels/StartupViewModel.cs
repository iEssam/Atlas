using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Threading;
using System.Threading.Tasks;
using Atlas.IpcClient;
using Atlas.V0;
using CommunityToolkit.Mvvm.ComponentModel;
using Microsoft.UI.Dispatching;

namespace Atlas.App.ViewModels;

/// <summary>
/// Drives the Startup page (M7, PRD §9.8.1): the auto-start inventory grouped by
/// source category (Run keys / Startup folders / Tasks / Services / Packaged),
/// each entry with name, command, publisher, enabled state, and scope.
/// Read-only this milestone — enable/disable arrives later via the broker — so
/// no action controls are shown. Degrades gracefully when the service is too old
/// (Unimplemented → inline "unavailable" placeholder).
/// </summary>
public sealed partial class StartupViewModel : ObservableObject
{
    private readonly DispatcherQueue _dispatcher;
    private readonly string? _who;
    private CancellationTokenSource? _cts;

    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private bool _isUnavailable;
    [ObservableProperty] private bool _hasLoaded;
    [ObservableProperty] private bool _isEmpty;
    [ObservableProperty] private string _statusText = string.Empty;

    /// <summary>Startup entries grouped by source category.</summary>
    public ObservableCollection<StartupGroup> Groups { get; } = new();

    /// <summary>Category display order for the grouped table.</summary>
    private static readonly string[] CategoryOrder =
    {
        "Run keys", "Startup folders", "Tasks", "Services", "Packaged", "Other",
    };

    public StartupViewModel(DispatcherQueue dispatcher, string? who = null)
    {
        _dispatcher = dispatcher;
        _who = who;
    }

    /// <summary>Loads (or reloads) the startup inventory.</summary>
    public async Task RefreshAsync()
    {
        _cts?.Cancel();
        var cts = new CancellationTokenSource();
        _cts = cts;
        var ct = cts.Token;

        IsLoading = true;
        IsUnavailable = false;
        IsEmpty = false;
        StatusText = "Loading…";

        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.ListStartupAsync(ct).ConfigureAwait(false);

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
                    HasLoaded = true;
                    StatusText = "Startup inventory unavailable — the service is too old.";
                    IsLoading = false;
                });
                return;
            }

            var byCategory = new Dictionary<string, List<StartupItem>>();
            foreach (var e in outcome.Value.Entries)
            {
                var category = M7Formatter.StartupCategory(e.Source);
                if (!byCategory.TryGetValue(category, out var list))
                {
                    list = new List<StartupItem>();
                    byCategory[category] = list;
                }
                list.Add(new StartupItem(
                    string.IsNullOrEmpty(e.Name) ? "(unnamed)" : e.Name,
                    M7Formatter.Truncate(e.Command, 90),
                    e.Command ?? string.Empty,
                    string.IsNullOrEmpty(e.Publisher) ? "—" : e.Publisher,
                    e.Enabled,
                    M7Formatter.EnabledLabel(e.Enabled),
                    string.IsNullOrEmpty(e.Scope) ? "—" : e.Scope,
                    M7Formatter.StartupSourceLabel(e.Source)));
            }

            Post(() =>
            {
                Groups.Clear();
                int total = 0;
                foreach (var category in CategoryOrder)
                {
                    if (!byCategory.TryGetValue(category, out var items) || items.Count == 0)
                    {
                        continue;
                    }
                    items.Sort((a, b) => string.Compare(a.Name, b.Name, StringComparison.OrdinalIgnoreCase));
                    var group = new StartupGroup(category);
                    foreach (var item in items)
                    {
                        group.Items.Add(item);
                        total++;
                    }
                    Groups.Add(group);
                }

                IsEmpty = total == 0;
                HasLoaded = true;
                StatusText = total == 0
                    ? "No startup entries found."
                    : $"{total} startup entr{(total == 1 ? "y" : "ies")} across {Groups.Count} source{(Groups.Count == 1 ? "" : "s")}.";
                IsLoading = false;
            });
        }
        catch (OperationCanceledException)
        {
            // Superseded by a newer refresh.
        }
        catch (Exception ex)
        {
            Post(() =>
            {
                Groups.Clear();
                IsUnavailable = true;
                HasLoaded = true;
                StatusText = $"Could not reach the service: {ex.Message}";
                IsLoading = false;
            });
        }
    }

    public void Stop() => _cts?.Cancel();

    private void Post(Action action) => _dispatcher.TryEnqueue(() => action());
}

/// <summary>A source-category group of startup entries.</summary>
public sealed class StartupGroup
{
    public string Title { get; }
    public ObservableCollection<StartupItem> Items { get; } = new();

    public StartupGroup(string title)
    {
        Title = title;
    }
}

/// <summary>One startup entry, pre-formatted for the read-only table.</summary>
public sealed class StartupItem
{
    public string Name { get; }
    public string CommandText { get; }
    public string CommandFull { get; }
    public string Publisher { get; }
    public bool Enabled { get; }
    public string EnabledText { get; }
    public string Scope { get; }
    public string SourceText { get; }

    public StartupItem(
        string name, string commandText, string commandFull, string publisher,
        bool enabled, string enabledText, string scope, string sourceText)
    {
        Name = name;
        CommandText = commandText;
        CommandFull = commandFull;
        Publisher = publisher;
        Enabled = enabled;
        EnabledText = enabledText;
        Scope = scope;
        SourceText = sourceText;
    }
}
