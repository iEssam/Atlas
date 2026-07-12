using System.Collections.ObjectModel;
using Atlas.App.Services;
using Atlas.IpcClient;
using CommunityToolkit.Mvvm.ComponentModel;
using Microsoft.UI.Dispatching;

namespace Atlas.App.ViewModels;

/// <summary>
/// Drives the Live Activity page. Data comes from <see cref="LiveMetricsService"/>,
/// which <b>prefers the shared-memory ring</b> and falls back to the gRPC
/// stream; the active source is shown subtly in the status line. Rows are
/// updated in place (matched by PID) so the virtualized list is not rebuilt
/// every tick. Capabilities are fetched once, best-effort, over gRPC (the ring
/// carries no capability metadata).
/// </summary>
public sealed partial class LiveActivityViewModel : ObservableObject
{
    private readonly DispatcherQueue _dispatcher;
    private readonly LiveMetricsService _metrics;
    private readonly Dictionary<uint, ProcessRowViewModel> _index = new();
    private readonly string? _who;

    private CancellationTokenSource? _cts;

    public ObservableCollection<ProcessRowViewModel> Processes { get; } = new();

    [ObservableProperty] private string _connectionStatus = "Disconnected";
    [ObservableProperty] private string _serviceVersion = "-";
    [ObservableProperty] private string _capabilityFlags = "-";

    [ObservableProperty] private double _systemCpuPercent;
    [ObservableProperty] private double _memUsedGb;
    [ObservableProperty] private double _memTotalGb;
    [ObservableProperty] private uint _processCount;
    [ObservableProperty] private uint _threadCount;
    [ObservableProperty] private uint _handleCount;

    /// <param name="dispatcher">The UI thread's dispatcher queue.</param>
    /// <param name="who">Pipe/ring discriminator (default: USERNAME) — matches
    /// the server's <c>--pipe</c> flag.</param>
    public LiveActivityViewModel(DispatcherQueue dispatcher, string? who = null)
    {
        _dispatcher = dispatcher;
        _who = who;
        _metrics = new LiveMetricsService(dispatcher, who);
        _metrics.StatusChanged += (_, status) => ConnectionStatus = status;
        _metrics.SnapshotReceived += Apply;
    }

    /// <summary>Connects and begins updating.</summary>
    public void Start()
    {
        _cts = new CancellationTokenSource();
        _ = FetchCapabilitiesAsync(_cts.Token);
        _metrics.Start();
    }

    /// <summary>Stops updating and releases resources.</summary>
    public void Stop()
    {
        _cts?.Cancel();
        _metrics.Stop();
    }

    /// <summary>
    /// Best-effort one-shot capabilities fetch over gRPC. The ring has no
    /// capability metadata, so this runs regardless of the active data source;
    /// a failure just leaves the capabilities line blank.
    /// </summary>
    private async Task FetchCapabilitiesAsync(CancellationToken ct)
    {
        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var caps = await channel.GetCapabilitiesAsync(ct).ConfigureAwait(false);
            Post(() =>
            {
                ServiceVersion = caps.ServiceVersion;
                CapabilityFlags = string.Join(", ", caps.CapabilityFlags);
            });
        }
        catch
        {
            // Capabilities are informational; ignore failures.
        }
    }

    /// <summary>Applies a snapshot on the UI thread: gauges + in-place row diff.</summary>
    private void Apply(MetricsSnapshot snap)
    {
        SystemCpuPercent = snap.CpuPercent;
        MemUsedGb = snap.MemUsed / (1024.0 * 1024.0 * 1024.0);
        MemTotalGb = snap.MemTotal / (1024.0 * 1024.0 * 1024.0);
        ProcessCount = snap.ProcessCount;
        ThreadCount = snap.ThreadCount;
        HandleCount = snap.HandleCount;

        var seen = new HashSet<uint>(snap.Rows.Count);

        // Rows are already sorted CPU-desc. Reconcile the collection to match
        // that order, updating existing VMs and inserting new ones.
        for (int i = 0; i < snap.Rows.Count; i++)
        {
            var row = snap.Rows[i];
            seen.Add(row.Pid);

            if (!_index.TryGetValue(row.Pid, out var vm))
            {
                vm = new ProcessRowViewModel(row.Pid, row.CreateTime100ns);
                vm.Update(row);
                _index[row.Pid] = vm;
                Processes.Insert(Math.Min(i, Processes.Count), vm);
            }
            else
            {
                vm.Update(row);
                int current = Processes.IndexOf(vm);
                int target = Math.Min(i, Processes.Count - 1);
                if (current >= 0 && current != target)
                {
                    Processes.Move(current, target);
                }
            }
        }

        // Drop rows for processes no longer present.
        for (int i = Processes.Count - 1; i >= 0; i--)
        {
            var vm = Processes[i];
            if (!seen.Contains(vm.Pid))
            {
                Processes.RemoveAt(i);
                _index.Remove(vm.Pid);
            }
        }
    }

    private void Post(Action action) => _dispatcher.TryEnqueue(() => action());
}
