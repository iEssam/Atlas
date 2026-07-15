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
/// Drives the Timeline page (M6, PRD §11.3): system CPU over a selectable window
/// as a decimated min/max band + avg line, plus an event lane and bookmark
/// markers. History is <b>not live</b> — it refreshes on window change and on a
/// manual refresh, never at 1 Hz. Every history RPC degrades gracefully when the
/// server is too old (returns <c>Unimplemented</c>): the page shows an inline
/// "history unavailable" placeholder rather than crashing.
/// </summary>
public sealed partial class TimelineViewModel : ObservableObject
{
    private readonly DispatcherQueue _dispatcher;
    private readonly string? _who;
    private CancellationTokenSource? _cts;

    /// <summary>Selectable windows (minutes) shown in the picker.</summary>
    public IReadOnlyList<TimelineWindow> Windows { get; } = new[]
    {
        new TimelineWindow("Last 10 minutes", TimeSpan.FromMinutes(10)),
        new TimelineWindow("Last 1 hour", TimeSpan.FromHours(1)),
    };
    public IReadOnlyList<TimelineMetric> Metrics { get; } = new[]
    {
        new TimelineMetric("System CPU", MetricKind.SysCpuPermille),
        new TimelineMetric("System GPU", MetricKind.SysGpuPermille),
    };

    [ObservableProperty] private TimelineWindow _selectedWindow;
    [ObservableProperty] private TimelineMetric _selectedMetric;
    [ObservableProperty] private bool _isLoading;

    /// <summary>True when the last query showed the server can't serve history.</summary>
    [ObservableProperty] private bool _isUnavailable;
    [ObservableProperty] private string _statusText = string.Empty;

    /// <summary>The percent-scaled CPU points (band + avg) for the chart canvas.</summary>
    public ObservableCollection<HistoryFormatter.TimelinePoint> CpuPoints { get; } = new();

    /// <summary>The expected bucket step (ms), so the renderer can find gaps.</summary>
    [ObservableProperty] private long _bucketStepMs;

    public ObservableCollection<TimelineEventItem> Events { get; } = new();
    public ObservableCollection<Bookmark> Bookmarks { get; } = new();

    /// <summary>The chart window bounds (ms epoch), so the canvas maps X.</summary>
    public long WindowFromMs { get; private set; }
    public long WindowToMs { get; private set; }

    /// <summary>Raised after a refresh completes so the view can redraw the canvas.</summary>
    public event Action? DataRefreshed;

    public TimelineViewModel(DispatcherQueue dispatcher, string? who = null)
    {
        _dispatcher = dispatcher;
        _who = who;
        _selectedWindow = Windows[0];
        _selectedMetric = Metrics[0];
    }

    partial void OnSelectedWindowChanged(TimelineWindow value) => _ = RefreshAsync();
    partial void OnSelectedMetricChanged(TimelineMetric value) => _ = RefreshAsync();

    /// <summary>Loads (or reloads) the selected window's history.</summary>
    public async Task RefreshAsync()
    {
        _cts?.Cancel();
        var cts = new CancellationTokenSource();
        _cts = cts;
        var ct = cts.Token;

        IsLoading = true;
        IsUnavailable = false;
        StatusText = "Loading…";

        var now = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
        var from = now - (long)SelectedWindow.Span.TotalMilliseconds;
        WindowFromMs = from;
        WindowToMs = now;

        try
        {
            using var channel = AtlasChannel.Connect(_who);

            // ~1 bucket per 2px of a ~900px chart; server clamps to its cap.
            const uint targetBuckets = 450;
            var cpu = await channel
                .QueryRangeAsync(SelectedMetric.Kind, 0, from, now, targetBuckets, ct)
                .ConfigureAwait(false);

            if (ct.IsCancellationRequested)
            {
                return;
            }

            if (!cpu.Supported)
            {
                Post(() =>
                {
                    IsUnavailable = true;
                    StatusText = "History unavailable — the service is too old to serve stored samples.";
                    CpuPoints.Clear();
                    Events.Clear();
                    Bookmarks.Clear();
                    IsLoading = false;
                    DataRefreshed?.Invoke();
                });
                return;
            }

            var points = HistoryFormatter.ToCpuTimeline(cpu.Value.Buckets);
            long step = EstimateStepMs(points, from, now);

            // Events + bookmarks are best-effort; either may be unsupported even
            // if QueryRange isn't. Missing lanes just render empty.
            var eventsOutcome = await channel
                .ListEventsAsync(from, now, limit: 500, cancellationToken: ct).ConfigureAwait(false);
            var bookmarksOutcome = await channel
                .ListBookmarksAsync(from, now, ct).ConfigureAwait(false);

            Post(() =>
            {
                CpuPoints.Clear();
                foreach (var p in points)
                {
                    CpuPoints.Add(p);
                }
                BucketStepMs = step;

                Events.Clear();
                if (eventsOutcome.Supported)
                {
                    foreach (var e in eventsOutcome.Value.Events)
                    {
                        Events.Add(new TimelineEventItem(
                            e.TsMs, e.Kind, HistoryFormatter.EventLine(e)));
                    }
                }

                Bookmarks.Clear();
                if (bookmarksOutcome.Supported)
                {
                    foreach (var b in bookmarksOutcome.Value.Bookmarks)
                    {
                        Bookmarks.Add(b);
                    }
                }

                StatusText = points.Count == 0
                    ? "No samples in this window yet."
                    : $"{points.Count} points • {Events.Count} events • {Bookmarks.Count} bookmarks";
                IsLoading = false;
                DataRefreshed?.Invoke();
            });
        }
        catch (OperationCanceledException)
        {
            // Superseded by a newer refresh — leave state to that call.
        }
        catch (Exception ex)
        {
            Post(() =>
            {
                IsUnavailable = true;
                StatusText = $"Could not reach the service: {ex.Message}";
                CpuPoints.Clear();
                Events.Clear();
                Bookmarks.Clear();
                IsLoading = false;
                DataRefreshed?.Invoke();
            });
        }
    }

    /// <summary>
    /// Places an incident bookmark at "now" and reloads so it appears. Returns a
    /// short status the view can surface. Degrades if unsupported.
    /// </summary>
    public async Task<string> BookmarkNowAsync(string label)
    {
        var ts = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel
                .CreateBookmarkAsync(ts, string.IsNullOrWhiteSpace(label) ? "Bookmark" : label)
                .ConfigureAwait(false);
            if (!outcome.Supported)
            {
                return "Bookmarks unavailable — the service is too old.";
            }
            await RefreshAsync().ConfigureAwait(false);
            return "Bookmark added.";
        }
        catch (Exception ex)
        {
            return $"Could not add bookmark: {ex.Message}";
        }
    }

    public void Stop() => _cts?.Cancel();

    /// <summary>
    /// Estimates the bucket step from adjacent points (median-ish: use the
    /// smallest positive delta, which reflects the server's decimation stride);
    /// falls back to window/bucketCount when there are too few points.
    /// </summary>
    private static long EstimateStepMs(
        IReadOnlyList<HistoryFormatter.TimelinePoint> points, long from, long to)
    {
        long best = long.MaxValue;
        for (int i = 1; i < points.Count; i++)
        {
            long d = points[i].StartMs - points[i - 1].StartMs;
            if (d > 0 && d < best)
            {
                best = d;
            }
        }
        if (best != long.MaxValue)
        {
            return best;
        }
        return points.Count > 0 ? Math.Max(1, (to - from) / Math.Max(1, points.Count)) : 0;
    }

    private void Post(Action action) => _dispatcher.TryEnqueue(() => action());
}

public sealed record TimelineMetric(string Label, MetricKind Kind)
{
    public override string ToString() => Label;
}

/// <summary>A selectable timeline window.</summary>
public sealed record TimelineWindow(string Label, TimeSpan Span)
{
    public override string ToString() => Label;
}

/// <summary>An event-lane item (start/stop), pre-formatted for display.</summary>
public sealed class TimelineEventItem
{
    public long TsMs { get; }
    public uint Kind { get; }
    public string Text { get; }
    public bool IsStart => Kind == 0;

    /// <summary>Segoe Fluent glyph: up-arrow for start, down-arrow for stop.</summary>
    public string IconGlyph => IsStart ? "" : "";

    public TimelineEventItem(long tsMs, uint kind, string text)
    {
        TsMs = tsMs;
        Kind = kind;
        Text = text;
    }
}
