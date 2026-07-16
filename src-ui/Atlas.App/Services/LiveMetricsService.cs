using System;
using System.Collections.Generic;
using System.Threading;
using System.Threading.Tasks;
using Atlas.IpcClient;
using Atlas.V0;
using Microsoft.UI.Dispatching;

namespace Atlas.App.Services;

/// <summary>
/// The shared source-selection engine behind both the Live Activity and
/// Overview pages. It <b>prefers the shared-memory ring</b> (the lock-free hot
/// path published by <c>serve</c>) and <b>falls back to the gRPC
/// <c>StreamSnapshots</c></b> when the ring is unavailable or its reads start
/// failing, retrying the ring occasionally so it reclaims the fast path when the
/// service comes back.
///
/// <para>
/// A single <see cref="DispatcherQueueTimer"/> ticks at ~1 Hz on the UI thread
/// and drives the state machine:
/// <list type="bullet">
/// <item><b>Ring</b>: read a snapshot each tick; on a run of failed reads, drop
///   to the stream fallback.</item>
/// <item><b>Stream</b>: a background gRPC stream pushes snapshots; the timer
///   periodically re-probes the ring and switches back on success.</item>
/// <item><b>Connecting</b>: no source yet; each tick tries the ring, then the
///   stream.</item>
/// </list>
/// All snapshots and status changes are raised on the UI thread (the timer and
/// the dispatcher-marshalled stream callback), so subscribers touch UI state
/// directly.
/// </para>
/// </summary>
public sealed class LiveMetricsService
{
    private readonly DispatcherQueue _dispatcher;
    private readonly string? _who;
    private readonly DispatcherQueueTimer _timer;
    private readonly bool _preferRing;

    private MetricsSource _source = MetricsSource.None;
    private string _status = "Disconnected";

    private MetricsRing? _ring;
    private int _ringFailures;

    private AtlasChannel? _channel;
    private CancellationTokenSource? _streamCts;
    private int _ringRetryCountdown;

    // A short run of failed ring reads before dropping to the stream. A live 1 Hz
    // writer rarely returns null; several in a row means the writer is gone.
    private const int RingFailureThreshold = 3;
    // Re-probe the ring every N stream ticks (~15 s) to reclaim the hot path.
    private const int RingRetryTicks = 15;

    /// <summary>Raised (on the UI thread) with each new snapshot.</summary>
    public event Action<MetricsSnapshot>? SnapshotReceived;

    /// <summary>Raised (on the UI thread) when the source or status text changes.</summary>
    public event Action<MetricsSource, string>? StatusChanged;

    public MetricsSource Source => _source;
    public string Status => _status;

    /// <param name="dispatcher">The UI thread's dispatcher queue.</param>
    /// <param name="who">Ring/pipe discriminator (default: USERNAME).</param>
    public LiveMetricsService(
        DispatcherQueue dispatcher,
        string? who = null,
        bool preferRing = true)
    {
        _dispatcher = dispatcher;
        _who = who;
        _preferRing = preferRing;
        _timer = dispatcher.CreateTimer();
        _timer.Interval = TimeSpan.FromSeconds(1);
        _timer.IsRepeating = true;
        _timer.Tick += (_, _) => OnTick();
    }

    /// <summary>Begins polling. Safe to call once per page visit.</summary>
    public void Start()
    {
        SetStatus(MetricsSource.None, "Connecting...");
        _timer.Start();
    }

    /// <summary>Stops polling and releases the ring + gRPC channel.</summary>
    public void Stop()
    {
        _timer.Stop();
        var streamCts = _streamCts;
        _streamCts = null;
        streamCts?.Cancel();
        streamCts?.Dispose();
        _channel?.Dispose();
        _channel = null;
        _ring?.Dispose();
        _ring = null;
        _source = MetricsSource.None;
    }

    private void OnTick()
    {
        switch (_source)
        {
            case MetricsSource.Ring:
                PollRing();
                break;

            case MetricsSource.Stream:
                // The stream pushes on its own; here we just re-probe the ring
                // occasionally to reclaim the hot path.
                if (_preferRing && --_ringRetryCountdown <= 0)
                {
                    _ringRetryCountdown = RingRetryTicks;
                    if (TryEnterRing())
                    {
                        return;
                    }
                }
                break;

            default:
                // Not connected: prefer the ring, else start the stream.
                if (!_preferRing || !TryEnterRing())
                {
                    EnsureStream();
                }
                break;
        }
    }

    /// <summary>
    /// Attempts to open the ring and switch to it. Returns true on success
    /// (tearing down any active stream), false if the ring is unavailable or
    /// incompatible (leaving the current source untouched).
    /// </summary>
    private bool TryEnterRing()
    {
        var result = MetricsRing.TryOpen(_who);
        if (!result.IsOpened)
        {
            return false;
        }

        // Verify we can actually read one snapshot before committing.
        var snap = result.Ring!.Snapshot();
        if (snap is null)
        {
            result.Ring.Dispose();
            return false;
        }

        // Tear down the stream fallback if it was running.
        var streamCts = _streamCts;
        _streamCts = null;
        streamCts?.Cancel();
        streamCts?.Dispose();
        _channel?.Dispose();
        _channel = null;

        _ring?.Dispose();
        _ring = result.Ring;
        _ringFailures = 0;
        SetStatus(MetricsSource.Ring, "Connected (ring)");
        Emit(FromRing(snap));
        return true;
    }

    private void PollRing()
    {
        MetricsRing? ring = _ring;
        if (ring is null)
        {
            FallBackToStream("ring closed");
            return;
        }

        RingSnapshot? snap;
        try
        {
            snap = ring.Snapshot();
        }
        catch
        {
            snap = null;
        }

        if (snap is null)
        {
            if (++_ringFailures >= RingFailureThreshold)
            {
                FallBackToStream("ring reads failing");
            }
            return;
        }

        _ringFailures = 0;
        Emit(FromRing(snap));
    }

    private void FallBackToStream(string reason)
    {
        _ring?.Dispose();
        _ring = null;
        _ringFailures = 0;
        _ringRetryCountdown = RingRetryTicks;
        SetStatus(MetricsSource.Stream, "Connecting (stream)...");
        EnsureStream();
    }

    /// <summary>Starts the gRPC stream fallback if it is not already running.</summary>
    private void EnsureStream()
    {
        if (_streamCts is not null)
        {
            return;
        }
        var cts = new CancellationTokenSource();
        _streamCts = cts;
        _ = RunStreamAsync(cts);
    }

    private async Task RunStreamAsync(CancellationTokenSource cts)
    {
        string? disconnectReason = null;
        try
        {
            var channel = AtlasChannel.Connect(_who);
            _channel = channel;

            await foreach (var reply in channel.StreamSnapshotsAsync(0, cts.Token).ConfigureAwait(false))
            {
                var snap = FromReply(reply);
                Post(() =>
                {
                    // A late stream message can arrive after we reclaimed the
                    // ring; ignore it unless the stream is still the source.
                    if (_source == MetricsSource.Stream || _source == MetricsSource.None)
                    {
                        SetStatus(
                            MetricsSource.Stream,
                            _preferRing ? "Connected (stream)" : "Connected (full stream)");
                        Emit(snap);
                    }
                });
            }
        }
        catch (OperationCanceledException)
        {
            // Normal shutdown / ring reclaim.
        }
        catch (Exception ex)
        {
            disconnectReason = $"Stream error: {ex.Message}";
        }
        finally
        {
            Post(() =>
            {
                // Ignore cleanup from an older stream after Stop(), a restart,
                // or a successful transition back to the ring.
                if (!ReferenceEquals(_streamCts, cts))
                {
                    return;
                }

                _streamCts = null;
                cts.Dispose();
                _channel?.Dispose();
                _channel = null;
                if (_source != MetricsSource.Ring)
                {
                    SetStatus(
                        MetricsSource.None,
                        disconnectReason ?? "Stream ended; reconnecting...");
                }
            });
        }
    }

    private static MetricsSnapshot FromRing(RingSnapshot s)
    {
        var rows = new List<MetricsRow>(s.Rows.Count);
        foreach (var r in s.Rows)
        {
            rows.Add(new MetricsRow(
                r.Pid,
                createTime100ns: 0, // the ring carries pid only
                r.Name,
                r.CpuPermille / 10.0,
                r.GpuPermille / 10.0,
                r.WorkingSet,
                r.PrivateBytes,
                r.ReadBps,
                r.WriteBps,
                threadCount: 0, // not carried in the ring rows
                handleCount: 0,
                r.GpuDedicatedBytes, r.GpuSharedBytes,
                appGroup: string.Empty));
        }
        return new MetricsSnapshot(
            s.TsMs,
            MetricsSource.Ring,
            s.CpuPermille / 10.0,
            s.GpuPermille / 10.0,
            s.MemUsed, s.MemTotal, s.CommitUsed, s.CommitLimit,
            s.ProcessCount, s.ThreadCount, s.HandleCount,
            s.GpuDedicatedUsed, s.GpuDedicatedBudget, s.GpuSharedUsed, s.GpuSharedBudget,
            rows);
    }

    private static MetricsSnapshot FromReply(SnapshotReply reply)
    {
        var rows = new List<MetricsRow>(reply.Processes.Count);
        foreach (var p in reply.Processes)
        {
            rows.Add(new MetricsRow(
                p.Pid,
                p.CreateTime100Ns,
                p.ImageName,
                p.CpuPermille / 10.0,
                p.GpuPermille / 10.0,
                p.WorkingSet,
                p.PrivateBytes,
                p.ReadBps,
                p.WriteBps,
                p.ThreadCount,
                p.HandleCount,
                p.GpuDedicatedBytes, p.GpuSharedBytes,
                p.AppGroup));
        }
        var sys = reply.System;
        return new MetricsSnapshot(
            sys?.TsMs ?? DateTimeOffset.UtcNow.ToUnixTimeMilliseconds(),
            MetricsSource.Stream,
            sys?.CpuPermille / 10.0 ?? 0,
            sys?.GpuPermille / 10.0 ?? 0,
            sys?.MemUsed ?? 0,
            sys?.MemTotal ?? 0,
            sys?.CommitUsed ?? 0,
            sys?.CommitLimit ?? 0,
            sys?.ProcessCount ?? 0,
            sys?.ThreadCount ?? 0,
            sys?.HandleCount ?? 0,
            sys?.GpuDedicatedUsed ?? 0,
            sys?.GpuDedicatedBudget ?? 0,
            sys?.GpuSharedUsed ?? 0,
            sys?.GpuSharedBudget ?? 0,
            rows);
    }

    private void SetStatus(MetricsSource source, string status)
    {
        _source = source;
        _status = status;
        StatusChanged?.Invoke(source, status);
    }

    private void Emit(MetricsSnapshot snap) => SnapshotReceived?.Invoke(snap);

    private void Post(Action action) => _dispatcher.TryEnqueue(() => action());
}
