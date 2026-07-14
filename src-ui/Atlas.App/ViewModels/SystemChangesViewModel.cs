using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Globalization;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using Atlas.IpcClient;
using Atlas.V0;
using CommunityToolkit.Mvvm.ComponentModel;
using Microsoft.UI.Dispatching;

namespace Atlas.App.ViewModels;

/// <summary>
/// Drives the System Changes page (R3, PRD §9.13) — the product's headline
/// "what changed?" surface. A timeline-ordered list (newest first) of recorded
/// changes over a selectable window (24h / 7d / 30d): app / driver / update /
/// service / startup / task / power / default-app changes, each with its subject,
/// before→after detail, publisher and responsible installer. A kind dropdown
/// filters server-side; a text box filters the loaded rows client-side.
///
/// <para>
/// Tone is deliberate: a change is <b>information</b>, not a threat, so the list is
/// scannable and calm and its category colors never borrow the danger palette
/// (task brief). Degrades gracefully when the service is too old (Unimplemented →
/// inline "unavailable" placeholder) and states an empty window plainly.
/// </para>
/// </summary>
public sealed partial class SystemChangesViewModel : ObservableObject
{
    private readonly DispatcherQueue _dispatcher;
    private readonly string? _who;
    private readonly Func<long> _nowMs;
    private CancellationTokenSource? _cts;

    /// <summary>Every row returned for the current window+kind, before the text filter.</summary>
    private readonly List<SystemChangeRowViewModel> _allRows = new();

    /// <summary>Selectable look-back windows (PRD §9.13).</summary>
    public IReadOnlyList<SystemChangesWindow> Windows { get; } = new[]
    {
        new SystemChangesWindow("Last 24 hours", TimeSpan.FromHours(24)),
        new SystemChangesWindow("Last 7 days", TimeSpan.FromDays(7)),
        new SystemChangesWindow("Last 30 days", TimeSpan.FromDays(30)),
    };

    /// <summary>Kind filter groups; empty <c>Kinds</c> means "all".</summary>
    public IReadOnlyList<SystemChangeFilter> KindFilters { get; } = new[]
    {
        new SystemChangeFilter("All changes", Array.Empty<SystemChangeKind>()),
        new SystemChangeFilter("Apps", new[]
        {
            SystemChangeKind.AppInstalled, SystemChangeKind.AppUpdated, SystemChangeKind.AppRemoved,
        }),
        new SystemChangeFilter("Drivers", new[]
        {
            SystemChangeKind.DriverInstalled, SystemChangeKind.DriverUpdated,
        }),
        new SystemChangeFilter("Windows updates", new[] { SystemChangeKind.WindowsUpdate }),
        new SystemChangeFilter("Services", new[]
        {
            SystemChangeKind.ServiceInstalled, SystemChangeKind.ServiceConfigChanged, SystemChangeKind.ServiceRemoved,
        }),
        new SystemChangeFilter("Startup items", new[]
        {
            SystemChangeKind.StartupAdded, SystemChangeKind.StartupRemoved,
        }),
        new SystemChangeFilter("Scheduled tasks", new[]
        {
            SystemChangeKind.ScheduledTaskAdded, SystemChangeKind.ScheduledTaskRemoved,
        }),
        new SystemChangeFilter("Power & defaults", new[]
        {
            SystemChangeKind.PowerPlanChanged, SystemChangeKind.DefaultAppChanged,
        }),
    };

    [ObservableProperty] private SystemChangesWindow _selectedWindow;
    [ObservableProperty] private SystemChangeFilter _selectedKindFilter;

    [ObservableProperty] private string _textFilter = string.Empty;

    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private bool _isUnavailable;
    [ObservableProperty] private bool _isEmpty;
    [ObservableProperty] private string _statusText = string.Empty;

    [ObservableProperty] private SystemChangeRowViewModel? _selectedChange;

    /// <summary>True when a row is selected, so the detail pane can show/hide.</summary>
    public bool HasSelection => SelectedChange is not null;

    public ObservableCollection<SystemChangeRowViewModel> Changes { get; } = new();

    public SystemChangesViewModel(DispatcherQueue dispatcher, string? who = null, Func<long>? nowMs = null)
    {
        _dispatcher = dispatcher;
        _who = who;
        _nowMs = nowMs ?? (() => DateTimeOffset.UtcNow.ToUnixTimeMilliseconds());
        _selectedWindow = Windows[0];
        _selectedKindFilter = KindFilters[0];
    }

    partial void OnSelectedChangeChanged(SystemChangeRowViewModel? value) =>
        OnPropertyChanged(nameof(HasSelection));

    partial void OnSelectedWindowChanged(SystemChangesWindow value) => _ = RefreshAsync();

    partial void OnSelectedKindFilterChanged(SystemChangeFilter value) => _ = RefreshAsync();

    partial void OnTextFilterChanged(string value) => ApplyTextFilter();

    private long WindowTo() => _nowMs();

    private long WindowFrom() => WindowTo() - (long)SelectedWindow.Span.TotalMilliseconds;

    /// <summary>Loads (or reloads) changes for the selected window and kind filter.</summary>
    public async Task RefreshAsync()
    {
        _cts?.Cancel();
        var cts = new CancellationTokenSource();
        _cts = cts;
        var ct = cts.Token;

        var from = WindowFrom();
        var to = WindowTo();
        var kinds = SelectedKindFilter.Kinds;

        IsLoading = true;
        IsUnavailable = false;
        IsEmpty = false;
        StatusText = "Looking for changes…";

        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel
                .ListSystemChangesAsync(from, to, kinds.Length == 0 ? null : kinds, limit: 500, ct)
                .ConfigureAwait(false);

            if (ct.IsCancellationRequested)
            {
                return;
            }

            if (!outcome.Supported)
            {
                Post(() =>
                {
                    _allRows.Clear();
                    Changes.Clear();
                    SelectedChange = null;
                    IsUnavailable = true;
                    StatusText = "System changes unavailable — the service is too old.";
                    IsLoading = false;
                });
                return;
            }

            var now = to;
            var truncated = outcome.Value.Truncated;
            var rows = outcome.Value.Changes
                .Select(c => SystemChangeRowViewModel.From(c, now))
                .ToList();

            Post(() =>
            {
                _allRows.Clear();
                _allRows.AddRange(rows);
                ApplyTextFilter();

                StatusText = BuildStatus(_allRows.Count, Changes.Count, truncated);
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
                _allRows.Clear();
                Changes.Clear();
                SelectedChange = null;
                IsUnavailable = true;
                StatusText = $"Could not reach the service: {ex.Message}";
                IsLoading = false;
            });
        }
    }

    /// <summary>
    /// Re-projects <see cref="_allRows"/> into <see cref="Changes"/> applying the
    /// case-insensitive text filter over subject / detail / publisher / kind. Cheap
    /// and local, so it runs on every keystroke without a round-trip.
    /// </summary>
    private void ApplyTextFilter()
    {
        var needle = (TextFilter ?? string.Empty).Trim();
        IEnumerable<SystemChangeRowViewModel> filtered = _allRows;
        if (needle.Length > 0)
        {
            filtered = _allRows.Where(r => r.SearchBlob.Contains(needle, StringComparison.OrdinalIgnoreCase));
        }

        Changes.Clear();
        foreach (var row in filtered)
        {
            Changes.Add(row);
        }
        SelectedChange = null;

        IsEmpty = Changes.Count == 0;
        if (!IsLoading && !IsUnavailable)
        {
            StatusText = BuildStatus(_allRows.Count, Changes.Count, false);
        }
    }

    private string BuildStatus(int total, int shown, bool truncated)
    {
        if (total == 0)
        {
            return "No changes recorded in this window.";
        }
        var needle = (TextFilter ?? string.Empty).Trim();
        if (needle.Length > 0)
        {
            return shown == 0
                ? $"No changes match “{needle}”."
                : $"{shown} of {total} change{(total == 1 ? "" : "s")} match “{needle}”.";
        }
        var suffix = truncated ? " (showing the most recent)." : ".";
        return $"{total} change{(total == 1 ? "" : "s")} recorded{suffix}";
    }

    public void Stop() => _cts?.Cancel();

    private void Post(Action action) => _dispatcher.TryEnqueue(() => action());
}

/// <summary>A selectable look-back window for the System Changes page.</summary>
public sealed record SystemChangesWindow(string Label, TimeSpan Span)
{
    public override string ToString() => Label;
}

/// <summary>A kind-filter group; an empty <see cref="Kinds"/> means "all kinds".</summary>
public sealed record SystemChangeFilter(string Label, SystemChangeKind[] Kinds)
{
    public override string ToString() => Label;
}

/// <summary>One system change, pre-formatted for the timeline list + detail pane.</summary>
public sealed class SystemChangeRowViewModel
{
    public long Id { get; }
    public string KindGlyph { get; }
    public string KindLabel { get; }
    public string CategoryToken { get; }
    public string Subject { get; }
    public string DetailText { get; }
    public string Publisher { get; }
    public string Responsible { get; }
    public string Provenance { get; }
    public string TimeText { get; }
    public string AbsoluteTimeText { get; }
    public string ReversibleLabel { get; }

    public bool HasDetail => DetailText.Length > 0;
    public bool HasProvenance => Provenance.Length > 0;
    public bool HasPublisher => Publisher.Length > 0;
    public bool HasResponsible => Responsible.Length > 0;
    public bool IsReversible => ReversibleLabel.Length > 0;

    /// <summary>Lowercase concatenation used by the client-side text filter.</summary>
    public string SearchBlob { get; }

    private SystemChangeRowViewModel(
        long id, string kindGlyph, string kindLabel, string categoryToken, string subject,
        string detailText, string publisher, string responsible, string provenance,
        string timeText, string absoluteTimeText, string reversibleLabel)
    {
        Id = id;
        KindGlyph = kindGlyph;
        KindLabel = kindLabel;
        CategoryToken = categoryToken;
        Subject = subject;
        DetailText = detailText;
        Publisher = publisher;
        Responsible = responsible;
        Provenance = provenance;
        TimeText = timeText;
        AbsoluteTimeText = absoluteTimeText;
        ReversibleLabel = reversibleLabel;
        SearchBlob = string.Join(
            " ", kindLabel, subject, detailText, publisher, responsible).ToLowerInvariant();
    }

    private static string LocalStamp(long ms) =>
        DateTimeOffset.FromUnixTimeMilliseconds(ms).ToLocalTime()
            .ToString("MMM d, HH:mm", CultureInfo.CurrentCulture);

    public static SystemChangeRowViewModel From(SystemChange c, long nowMs)
    {
        var subject = string.IsNullOrWhiteSpace(c.Subject)
            ? R3Formatter.SystemChangeKindLabel(c.Kind)
            : c.Subject;
        return new SystemChangeRowViewModel(
            c.Id,
            R3Formatter.SystemChangeKindGlyph(c.Kind),
            R3Formatter.SystemChangeKindLabel(c.Kind),
            R3Formatter.SystemChangeCategoryToken(c.Kind),
            subject,
            (c.Detail ?? string.Empty).Trim(),
            (c.Publisher ?? string.Empty).Trim(),
            (c.Responsible ?? string.Empty).Trim(),
            R3Formatter.ChangeProvenance(c.Publisher, c.Responsible),
            "Recorded " + M7Formatter.RelativeTime(c.TsMs, nowMs),
            LocalStamp(c.TsMs),
            R3Formatter.ReversibleLabel(c.Reversible));
    }
}
