using System.Collections.ObjectModel;
using Atlas.App.Services;
using CommunityToolkit.Mvvm.ComponentModel;
using Microsoft.UI.Dispatching;

namespace Atlas.App.ViewModels;

/// <summary>
/// Drives the Overview page: a card row of system gauges plus a "Top consumers"
/// list (top 5 by CPU). Same ring-preferred source selection as Live Activity
/// (via <see cref="LiveMetricsService"/>). Presents only measured values — no
/// health verdicts (PRD §9.1).
/// </summary>
public sealed partial class OverviewViewModel : ObservableObject
{
    private const int TopConsumers = 5;

    private readonly LiveMetricsService _metrics;

    public ObservableCollection<ConsumerRowViewModel> TopConsumers5 { get; } = new();

    [ObservableProperty] private string _connectionStatus = "Disconnected";

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(CpuText))]
    private double _cpuPercent;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(MemoryText))]
    [NotifyPropertyChangedFor(nameof(MemoryPercent))]
    private double _memUsedGb;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(MemoryText))]
    [NotifyPropertyChangedFor(nameof(MemoryPercent))]
    private double _memTotalGb;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(CommitText))]
    private double _commitUsedGb;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(CommitText))]
    private double _commitLimitGb;

    [ObservableProperty] private uint _processCount;
    [ObservableProperty] private uint _threadCount;
    [ObservableProperty] private uint _handleCount;

    public string CpuText => $"{CpuPercent:F1} %";
    public string MemoryText => $"{MemUsedGb:F1} / {MemTotalGb:F1} GB";
    /// <summary>Memory used as a 0..100 percentage for the progress bar.</summary>
    public double MemoryPercent => MemTotalGb > 0 ? MemUsedGb / MemTotalGb * 100.0 : 0;
    public string CommitText => $"{CommitUsedGb:F1} / {CommitLimitGb:F1} GB";

    public OverviewViewModel(DispatcherQueue dispatcher, string? who = null)
    {
        _metrics = new LiveMetricsService(dispatcher, who);
        _metrics.StatusChanged += (_, status) => ConnectionStatus = status;
        _metrics.SnapshotReceived += Apply;
    }

    public void Start() => _metrics.Start();

    public void Stop() => _metrics.Stop();

    private void Apply(MetricsSnapshot snap)
    {
        const double giga = 1024.0 * 1024.0 * 1024.0;
        CpuPercent = snap.CpuPercent;
        MemUsedGb = snap.MemUsed / giga;
        MemTotalGb = snap.MemTotal / giga;
        CommitUsedGb = snap.CommitUsed / giga;
        CommitLimitGb = snap.CommitLimit / giga;
        ProcessCount = snap.ProcessCount;
        ThreadCount = snap.ThreadCount;
        HandleCount = snap.HandleCount;

        // Top-5 by CPU (rows already CPU-desc). Reconcile in place so the small
        // list does not flicker: update the first N, add/remove to match count.
        int n = Math.Min(TopConsumers, snap.Rows.Count);
        for (int i = 0; i < n; i++)
        {
            var row = snap.Rows[i];
            if (i < TopConsumers5.Count)
            {
                TopConsumers5[i].Update(row);
            }
            else
            {
                var vm = new ConsumerRowViewModel();
                vm.Update(row);
                TopConsumers5.Add(vm);
            }
        }
        for (int i = TopConsumers5.Count - 1; i >= n; i--)
        {
            TopConsumers5.RemoveAt(i);
        }
    }
}
