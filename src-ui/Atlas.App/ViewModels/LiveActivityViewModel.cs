using System.Collections.ObjectModel;
using Atlas.IpcClient;
using Atlas.V0;
using CommunityToolkit.Mvvm.ComponentModel;
using Microsoft.UI.Dispatching;

namespace Atlas.App.ViewModels;

/// <summary>
/// Drives the Live Activity page: subscribes to <c>StreamSnapshots</c> and
/// marshals each ~1 Hz update onto the UI thread via the
/// <see cref="DispatcherQueue"/>. Rows are updated in place (matched by PID +
/// create-time) so the virtualized list is not rebuilt every tick.
/// </summary>
public sealed partial class LiveActivityViewModel : ObservableObject
{
    private readonly DispatcherQueue _dispatcher;
    private readonly Dictionary<(uint Pid, long Ct), ProcessRowViewModel> _index = new();

    private CancellationTokenSource? _cts;
    private AtlasChannel? _channel;
    private readonly string? _who;

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
    /// <param name="who">Pipe discriminator (default: USERNAME) — matches the
    /// server's <c>--pipe</c> flag.</param>
    public LiveActivityViewModel(DispatcherQueue dispatcher, string? who = null)
    {
        _dispatcher = dispatcher;
        _who = who;
    }

    /// <summary>Connects and begins streaming. Idempotent-ish: safe to call once.</summary>
    public void Start()
    {
        _cts = new CancellationTokenSource();
        _ = RunAsync(_cts.Token);
    }

    /// <summary>Stops streaming and releases the channel.</summary>
    public void Stop()
    {
        _cts?.Cancel();
        _channel?.Dispose();
        _channel = null;
    }

    private async Task RunAsync(CancellationToken ct)
    {
        try
        {
            Post(() => ConnectionStatus = "Connecting...");
            _channel = AtlasChannel.Connect(_who);

            var caps = await _channel.GetCapabilitiesAsync(ct).ConfigureAwait(false);
            Post(() =>
            {
                ServiceVersion = caps.ServiceVersion;
                CapabilityFlags = string.Join(", ", caps.CapabilityFlags);
                ConnectionStatus = "Connected";
            });

            await foreach (var reply in _channel.StreamSnapshotsAsync(0, ct).ConfigureAwait(false))
            {
                Post(() => Apply(reply));
            }
        }
        catch (OperationCanceledException)
        {
            // Normal shutdown.
        }
        catch (Exception ex)
        {
            Post(() => ConnectionStatus = $"Error: {ex.Message}");
        }
    }

    /// <summary>Applies a snapshot on the UI thread: gauges + in-place row diff.</summary>
    private void Apply(SnapshotReply reply)
    {
        if (reply.System is { } s)
        {
            SystemCpuPercent = s.CpuPermille / 10.0;
            MemUsedGb = s.MemUsed / (1024.0 * 1024.0 * 1024.0);
            MemTotalGb = s.MemTotal / (1024.0 * 1024.0 * 1024.0);
            ProcessCount = s.ProcessCount;
            ThreadCount = s.ThreadCount;
            HandleCount = s.HandleCount;
        }

        var seen = new HashSet<(uint, long)>(reply.Processes.Count);

        // Server rows are already sorted CPU-desc. Reconcile the collection to
        // match that order, updating existing VMs and inserting new ones.
        for (int i = 0; i < reply.Processes.Count; i++)
        {
            var row = reply.Processes[i];
            var key = (row.Pid, row.CreateTime100Ns);
            seen.Add(key);

            if (!_index.TryGetValue(key, out var vm))
            {
                vm = new ProcessRowViewModel(row.Pid, row.CreateTime100Ns);
                vm.Update(row);
                _index[key] = vm;
                if (i <= Processes.Count)
                {
                    Processes.Insert(Math.Min(i, Processes.Count), vm);
                }
                else
                {
                    Processes.Add(vm);
                }
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
            if (!seen.Contains((vm.Pid, vm.CreateTime100ns)))
            {
                Processes.RemoveAt(i);
                _index.Remove((vm.Pid, vm.CreateTime100ns));
            }
        }
    }

    private void Post(Action action) => _dispatcher.TryEnqueue(() => action());
}
