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
/// Drives the Reliability page (R3, PRD §9.14) — a calm history of crash / hang /
/// bugcheck / service-failure / unexpected-shutdown records over a selectable
/// window, each shown with its kind, subject, fault, time, and the correlated
/// <b>context</b> the service assembled around it (peak memory before, a recent
/// update, a repeated-restart note).
///
/// <para>
/// The framing mirrors Diagnostics' epistemic honesty: context is <b>correlation,
/// not blame</b>, rendered as hedged bullet points, and the color tokens top out
/// at caution — never critical. This page also honors <b>two</b> distinct honest
/// "nothing to show" states: the transport <see cref="IsUnsupported"/> (the service
/// is too old to serve the RPC) and the in-band <see cref="IsUnavailable"/> (the
/// reply says <c>available = false</c> — e.g. "reliability log unavailable"). An
/// empty-but-available window is framed as good news, not an error.
/// </para>
/// </summary>
public sealed partial class ReliabilityViewModel : ObservableObject
{
    private readonly DispatcherQueue _dispatcher;
    private readonly string? _who;
    private readonly Func<long> _nowMs;
    private CancellationTokenSource? _cts;

    /// <summary>Selectable look-back windows.</summary>
    public IReadOnlyList<ReliabilityWindow> Windows { get; } = new[]
    {
        new ReliabilityWindow("Last 24 hours", TimeSpan.FromHours(24)),
        new ReliabilityWindow("Last 7 days", TimeSpan.FromDays(7)),
        new ReliabilityWindow("Last 30 days", TimeSpan.FromDays(30)),
    };

    /// <summary>Kind filter groups; empty <c>Kinds</c> means "all".</summary>
    public IReadOnlyList<CrashFilter> KindFilters { get; } = new[]
    {
        new CrashFilter("All events", Array.Empty<CrashKind>()),
        new CrashFilter("App crashes", new[] { CrashKind.AppCrash }),
        new CrashFilter("App hangs", new[] { CrashKind.AppHang }),
        new CrashFilter("Bugchecks", new[] { CrashKind.Bugcheck }),
        new CrashFilter("Service failures", new[] { CrashKind.ServiceFailure }),
        new CrashFilter("Shutdowns", new[] { CrashKind.UnexpectedShutdown }),
    };

    [ObservableProperty] private ReliabilityWindow _selectedWindow;
    [ObservableProperty] private CrashFilter _selectedKindFilter;

    [ObservableProperty] private bool _isLoading;

    /// <summary>Transport-level: the service is too old to serve ListCrashes at all.</summary>
    [ObservableProperty] private bool _isUnsupported;

    /// <summary>In-band: the reply reported <c>available = false</c> with a reason.</summary>
    [ObservableProperty] private bool _isUnavailable;

    /// <summary>Available, but no records fell in the window — good news, framed calmly.</summary>
    [ObservableProperty] private bool _isEmpty;

    [ObservableProperty] private string _statusText = string.Empty;

    /// <summary>The service's plain reason when <see cref="IsUnavailable"/> (or a transport error).</summary>
    [ObservableProperty] private string _unavailableMessage = string.Empty;

    public ObservableCollection<CrashRowViewModel> Crashes { get; } = new();

    /// <summary>The standing hedged heading for correlated-context lists.</summary>
    public string ContextIntro => R3Formatter.ContextIntro;

    public ReliabilityViewModel(DispatcherQueue dispatcher, string? who = null, Func<long>? nowMs = null)
    {
        _dispatcher = dispatcher;
        _who = who;
        _nowMs = nowMs ?? (() => DateTimeOffset.UtcNow.ToUnixTimeMilliseconds());
        _selectedWindow = Windows[0];
        _selectedKindFilter = KindFilters[0];
    }

    partial void OnSelectedWindowChanged(ReliabilityWindow value) => _ = RefreshAsync();

    partial void OnSelectedKindFilterChanged(CrashFilter value) => _ = RefreshAsync();

    private long WindowTo() => _nowMs();

    private long WindowFrom() => WindowTo() - (long)SelectedWindow.Span.TotalMilliseconds;

    /// <summary>Loads (or reloads) reliability records for the selected window and kinds.</summary>
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
        IsUnsupported = false;
        IsUnavailable = false;
        IsEmpty = false;
        UnavailableMessage = string.Empty;
        StatusText = "Looking for reliability events…";

        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel
                .ListCrashesAsync(from, to, kinds.Length == 0 ? null : kinds, limit: 200, ct)
                .ConfigureAwait(false);

            if (ct.IsCancellationRequested)
            {
                return;
            }

            // Transport: server too old to serve the RPC.
            if (!outcome.Supported)
            {
                Post(() =>
                {
                    Crashes.Clear();
                    IsUnsupported = true;
                    UnavailableMessage =
                        "The connected service is too old to report reliability events. Update the service to see them.";
                    StatusText = "Reliability unavailable — the service is too old.";
                    IsLoading = false;
                });
                return;
            }

            var reply = outcome.Value;

            // In-band: the reliability log itself is unavailable on this machine.
            if (!reply.Available)
            {
                Post(() =>
                {
                    Crashes.Clear();
                    IsUnavailable = true;
                    UnavailableMessage = string.IsNullOrWhiteSpace(reply.UnavailableReason)
                        ? "The reliability log is unavailable on this machine."
                        : reply.UnavailableReason;
                    StatusText = "Reliability log unavailable.";
                    IsLoading = false;
                });
                return;
            }

            var now = to;
            var truncated = reply.Truncated;
            var rows = reply.Crashes.Select(c => CrashRowViewModel.From(c, now)).ToList();

            Post(() =>
            {
                Crashes.Clear();
                foreach (var row in rows)
                {
                    Crashes.Add(row);
                }

                IsEmpty = Crashes.Count == 0;
                StatusText = Crashes.Count == 0
                    ? "No reliability events recorded in this window."
                    : $"{Crashes.Count} reliability event{(Crashes.Count == 1 ? "" : "s")}"
                        + (truncated ? " (showing the most recent)." : ".");
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
                Crashes.Clear();
                IsUnavailable = true;
                UnavailableMessage = $"Could not reach the service: {ex.Message}";
                StatusText = "Could not reach the service.";
                IsLoading = false;
            });
        }
    }

    public void Stop() => _cts?.Cancel();

    private void Post(Action action) => _dispatcher.TryEnqueue(() => action());
}

/// <summary>A selectable look-back window for the Reliability page.</summary>
public sealed record ReliabilityWindow(string Label, TimeSpan Span)
{
    public override string ToString() => Label;
}

/// <summary>A crash-kind filter group; an empty <see cref="Kinds"/> means "all".</summary>
public sealed record CrashFilter(string Label, CrashKind[] Kinds)
{
    public override string ToString() => Label;
}

/// <summary>One reliability record, pre-formatted with its correlated context.</summary>
public sealed class CrashRowViewModel
{
    public long Id { get; }
    public string KindGlyph { get; }
    public string KindLabel { get; }
    public string CautionToken { get; }
    public string Subject { get; }
    public string FaultLine { get; }
    public string TimeText { get; }
    public string AbsoluteTimeText { get; }
    public IReadOnlyList<string> Context { get; }

    public bool HasFault => FaultLine.Length > 0;
    public bool HasContext => Context.Count > 0;

    private CrashRowViewModel(
        long id, string kindGlyph, string kindLabel, string cautionToken, string subject,
        string faultLine, string timeText, string absoluteTimeText, IReadOnlyList<string> context)
    {
        Id = id;
        KindGlyph = kindGlyph;
        KindLabel = kindLabel;
        CautionToken = cautionToken;
        Subject = subject;
        FaultLine = faultLine;
        TimeText = timeText;
        AbsoluteTimeText = absoluteTimeText;
        Context = context;
    }

    private static string LocalStamp(long ms) =>
        DateTimeOffset.FromUnixTimeMilliseconds(ms).ToLocalTime()
            .ToString("MMM d, HH:mm", CultureInfo.CurrentCulture);

    public static CrashRowViewModel From(CrashRecord c, long nowMs)
    {
        var context = c.Context
            .Select(R3Formatter.CrashContextLine)
            .ToList();
        return new CrashRowViewModel(
            c.Id,
            R3Formatter.CrashKindGlyph(c.Kind),
            R3Formatter.CrashKindLabel(c.Kind),
            R3Formatter.CrashCautionToken(c.Kind),
            R3Formatter.CrashSubjectText(c.Kind, c.Subject),
            R3Formatter.CrashFaultLine(c.Fault, c.ExceptionCode),
            M7Formatter.RelativeTime(c.TsMs, nowMs),
            LocalStamp(c.TsMs),
            context);
    }
}
