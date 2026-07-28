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
/// Drives the Privacy page (M7, PRD §9.10): per-app usage of privacy-sensitive
/// capabilities (Camera / Microphone / Location), grouped by capability, plus an
/// optional recent-activity list. The presentation is calm and factual and never
/// implies malice (§9.10.3): "in use now" and relative last-used times describe
/// access, not intent. Degrades gracefully when the service is too old to serve
/// these RPCs (Unimplemented → an inline "unavailable" placeholder).
/// </summary>
public sealed partial class PrivacyViewModel : ObservableObject
{
    private readonly DispatcherQueue _dispatcher;
    private readonly string? _who;
    private CancellationTokenSource? _cts;

    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private bool _isUnavailable;
    [ObservableProperty] private bool _hasLoaded;
    [ObservableProperty] private string _statusText = string.Empty;

    /// <summary>True once a successful load found no tracked usage at all.</summary>
    [ObservableProperty] private bool _isEmpty;

    /// <summary>Usage grouped by capability (Camera / Microphone / Location).</summary>
    public ObservableCollection<PrivacyCapabilityGroup> Groups { get; } = new();

    /// <summary>Recent start/stop transitions, when the service serves them.</summary>
    public ObservableCollection<PrivacyEventItem> RecentActivity { get; } = new();

    /// <summary>True when the recent-activity list is available and non-empty.</summary>
    [ObservableProperty] private bool _hasRecentActivity;

    public PrivacyViewModel(DispatcherQueue dispatcher, string? who = null)
    {
        _dispatcher = dispatcher;
        _who = who;
    }

    /// <summary>Loads (or reloads) current privacy usage and recent activity.</summary>
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

        long now = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();

        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var usage = await channel.ListPrivacyUsageAsync(cancellationToken: ct)
                .ConfigureAwait(false);

            if (ct.IsCancellationRequested)
            {
                return;
            }

            if (!usage.Supported)
            {
                Post(() =>
                {
                    Groups.Clear();
                    RecentActivity.Clear();
                    HasRecentActivity = false;
                    IsUnavailable = true;
                    HasLoaded = true;
                    StatusText = "Privacy activity unavailable — the service is too old.";
                    IsLoading = false;
                });
                return;
            }

            // Recent activity is best-effort: it may be unsupported even when
            // usage isn't. A missing list just renders nothing.
            var events = await channel
                .ListPrivacyEventsAsync(now - (long)TimeSpan.FromHours(24).TotalMilliseconds, now,
                    limit: 100, cancellationToken: ct)
                .ConfigureAwait(false);

            // Bucket usage by capability, ordered Camera, Microphone, Location.
            var order = new[]
            {
                CapabilityKind.Camera, CapabilityKind.Microphone, CapabilityKind.Location,
            };
            var byCapability = new Dictionary<CapabilityKind, List<PrivacyUsageItem>>();
            foreach (var cap in order)
            {
                byCapability[cap] = new List<PrivacyUsageItem>();
            }

            var aggregates = PrivacyUsageAggregator.Aggregate(usage.Value.Usages);
            foreach (var aggregate in aggregates)
            {
                if (!byCapability.TryGetValue(aggregate.Capability, out var list))
                {
                    list = new List<PrivacyUsageItem>();
                    byCapability[aggregate.Capability] = list;
                }
                var type = M7Formatter.PackagedLabel(aggregate.Packaged);
                var sourceText = aggregate.RecordCount == 1
                    ? type
                    : $"{type}, {aggregate.RecordCount} records combined";
                list.Add(new PrivacyUsageItem(
                    aggregate.DisplayName,
                    aggregate.AppId,
                    M7Formatter.UsageStatus(
                        aggregate.InUse,
                        aggregate.LastStartMs,
                        aggregate.LastStopMs,
                        now),
                    aggregate.InUse,
                    sourceText));
            }

            int total = aggregates.Count;

            Post(() =>
            {
                Groups.Clear();
                foreach (var cap in order)
                {
                    var items = byCapability[cap];
                    // Put active apps first, then keep the consolidated list easy
                    // to scan instead of exposing ConsentStore record order.
                    items.Sort((a, b) =>
                    {
                        var active = b.InUse.CompareTo(a.InUse);
                        return active != 0
                            ? active
                            : StringComparer.CurrentCultureIgnoreCase.Compare(a.DisplayName, b.DisplayName);
                    });
                    var group = new PrivacyCapabilityGroup(
                        M7Formatter.CapabilityLabel(cap),
                        M7Formatter.CapabilityGlyph(cap));
                    foreach (var item in items)
                    {
                        group.Items.Add(item);
                    }
                    Groups.Add(group);
                }

                RecentActivity.Clear();
                if (events.Supported)
                {
                    foreach (var e in events.Value.Events)
                    {
                        RecentActivity.Add(new PrivacyEventItem(
                            M7Formatter.PrivacyEventLine(e),
                            FormatTs(e.TsMs),
                            e.Started));
                    }
                }
                HasRecentActivity = RecentActivity.Count > 0;

                IsEmpty = total == 0;
                HasLoaded = true;
                StatusText = total == 0
                    ? "No camera, microphone, or location usage recorded yet."
                    : $"{total} app{(total == 1 ? "" : "s")} across camera, microphone, and location.";
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
                RecentActivity.Clear();
                HasRecentActivity = false;
                IsUnavailable = true;
                HasLoaded = true;
                StatusText = $"Could not reach the service: {ex.Message}";
                IsLoading = false;
            });
        }
    }

    public void Stop() => _cts?.Cancel();

    private static string FormatTs(long tsMs) =>
        DateTimeOffset.FromUnixTimeMilliseconds(tsMs).LocalDateTime.ToString("g");

    private void Post(Action action) => _dispatcher.TryEnqueue(() => action());
}

/// <summary>A capability's group of app usages, with a header glyph.</summary>
public sealed class PrivacyCapabilityGroup
{
    public string Title { get; }
    public string Glyph { get; }
    public ObservableCollection<PrivacyUsageItem> Items { get; } = new();

    /// <summary>Shown when a capability has no recorded usage.</summary>
    public string EmptyText => $"No {Title.ToLowerInvariant()} usage recorded.";

    public PrivacyCapabilityGroup(string title, string glyph)
    {
        Title = title;
        Glyph = glyph;
    }
}

/// <summary>One app's usage of a capability: name, status, packaged indicator.</summary>
public sealed class PrivacyUsageItem
{
    public string DisplayName { get; }
    public string AppId { get; }
    public string StatusText { get; }
    public bool InUse { get; }
    public string PackagedText { get; }

    public PrivacyUsageItem(string displayName, string appId, string statusText, bool inUse, string packagedText)
    {
        DisplayName = displayName;
        AppId = appId;
        StatusText = statusText;
        InUse = inUse;
        PackagedText = packagedText;
    }
}

/// <summary>A recent privacy transition line (start/stop), pre-formatted.</summary>
public sealed class PrivacyEventItem
{
    public string Text { get; }
    public string TimeText { get; }
    public bool Started { get; }

    /// <summary>Segoe Fluent glyph: filled dot for start, hollow for stop.</summary>
    public string Glyph => Started ? "" : "";

    public PrivacyEventItem(string text, string timeText, bool started)
    {
        Text = text;
        TimeText = timeText;
        Started = started;
    }
}
