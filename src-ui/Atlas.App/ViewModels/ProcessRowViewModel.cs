using System.Globalization;
using Atlas.App.Services;
using CommunityToolkit.Mvvm.ComponentModel;

namespace Atlas.App.ViewModels;

/// <summary>The stable identity of one Windows process instance.</summary>
public readonly record struct ProcessIdentity(uint Pid, long CreateTime100ns);

/// <summary>One live sample retained for the selected-process trace.</summary>
public readonly record struct ProcessLiveSample(
    long TimestampMs,
    double CpuPercent,
    double GpuPercent,
    double WorkingSetMb,
    double PrivateMb,
    double DiskMbPerSecond);

/// <summary>
/// One stable row in the Activity watchboard. Values update in place while the
/// row's position remains unchanged until the user explicitly sorts the list.
/// A ten-minute client-side sample window supplies immediate trends without
/// adding a history query to the one-second live path.
/// </summary>
public sealed partial class ProcessRowViewModel : ObservableObject
{
    private static readonly CultureInfo Inv = CultureInfo.InvariantCulture;
    private readonly Queue<ProcessLiveSample> _samples = new();

    public ProcessIdentity Identity { get; }
    public uint Pid => Identity.Pid;
    public long CreateTime100ns => Identity.CreateTime100ns;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(AccessibleSummary))]
    private string _imageName = string.Empty;

    [ObservableProperty]
    private string _appGroup = string.Empty;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(CpuText))]
    [NotifyPropertyChangedFor(nameof(AccessibleSummary))]
    private double _cpuPercent;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(GpuText))]
    [NotifyPropertyChangedFor(nameof(AccessibleSummary))]
    private double _gpuPercent;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(GpuMemoryText))]
    private double _gpuMemoryMb;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(WorkingSetText))]
    [NotifyPropertyChangedFor(nameof(AccessibleSummary))]
    private double _workingSetMb;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(PrivateText))]
    private double _privateMb;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(DiskText))]
    [NotifyPropertyChangedFor(nameof(AccessibleSummary))]
    private double _diskMbPerSecond;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(ThreadText))]
    private uint _threadCount;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(HandleText))]
    private uint _handleCount;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(ThreadText))]
    [NotifyPropertyChangedFor(nameof(HandleText))]
    private bool _hasDetailedCounts;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(TrackActionText))]
    [NotifyPropertyChangedFor(nameof(TrackGlyph))]
    [NotifyPropertyChangedFor(nameof(StatusText))]
    [NotifyPropertyChangedFor(nameof(AccessibleSummary))]
    private bool _isTracked;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(StatusText))]
    [NotifyPropertyChangedFor(nameof(AccessibleSummary))]
    private bool _isRunning = true;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(LastSeenText))]
    private long _lastSeenMs;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(AverageCpuText))]
    private double _averageCpuOneMinute;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(PeakCpuText))]
    private double _peakCpuOneMinute;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(MemoryDeltaText))]
    private double _memoryDeltaOneMinuteMb;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(CpuTrendText))]
    private double _cpuDeltaOneMinute;

    /// <summary>Raised after a sample is appended; used only by the selected trace.</summary>
    public event Action<ProcessRowViewModel>? SamplesChanged;

    public string CpuText => CpuPercent.ToString("F1", Inv);
    public string GpuText => GpuPercent.ToString("F1", Inv);
    public string GpuMemoryText => GpuMemoryMb.ToString("F0", Inv);
    public string WorkingSetText => WorkingSetMb.ToString("F1", Inv);
    public string PrivateText => PrivateMb.ToString("F1", Inv);
    public string DiskText => DiskMbPerSecond < 0.05
        ? "0"
        : DiskMbPerSecond.ToString("F1", Inv);
    public string ThreadText => HasDetailedCounts ? ThreadCount.ToString(Inv) : "\u2014";
    public string HandleText => HasDetailedCounts ? HandleCount.ToString(Inv) : "\u2014";
    public string AverageCpuText => AverageCpuOneMinute.ToString("F1", Inv);
    public string PeakCpuText => PeakCpuOneMinute.ToString("F1", Inv);
    public string MemoryDeltaText => FormatSigned(MemoryDeltaOneMinuteMb, " MB");
    public string CpuTrendText => Math.Abs(CpuDeltaOneMinute) < 0.2
        ? "Stable over 1 minute"
        : CpuDeltaOneMinute > 0
            ? $"Up {CpuDeltaOneMinute:F1} points in 1 minute"
            : $"Down {Math.Abs(CpuDeltaOneMinute):F1} points in 1 minute";
    public string TrackActionText => IsTracked ? "Stop tracking application" : "Track application";
    public string TrackGlyph => IsTracked ? "\uE735" : "\uE734";
    public string StatusText => IsRunning
        ? (IsTracked ? "Tracked, running" : "Running")
        : "Ended";
    public string LastSeenText => LastSeenMs <= 0
        ? "Waiting for samples"
        : $"Last sample {DateTimeOffset.FromUnixTimeMilliseconds(LastSeenMs):t}";
    public string AccessibleSummary =>
        $"{ImageName}, PID {Pid}, CPU {CpuText} percent, GPU {GpuText} percent, " +
        $"memory {WorkingSetText} megabytes, disk {DiskText} megabytes per second, {StatusText}.";

    public override string ToString() => AccessibleSummary;

    public ProcessRowViewModel(uint pid, long createTime100ns)
    {
        Identity = new ProcessIdentity(pid, createTime100ns);
    }

    /// <summary>Returns a copy so the renderer never observes a mutating queue.</summary>
    public IReadOnlyList<ProcessLiveSample> GetSamples() => _samples.ToArray();

    /// <summary>Updates measured values and rolls the local trend window.</summary>
    public void Update(MetricsRow row, long timestampMs, bool detailedCountsAvailable)
    {
        ImageName = row.ImageName;
        AppGroup = row.AppGroup;
        CpuPercent = Sanitize(row.CpuPercent);
        GpuPercent = Sanitize(row.GpuPercent);
        GpuMemoryMb = BytesToMb(row.GpuDedicatedBytes + row.GpuSharedBytes);
        WorkingSetMb = BytesToMb(row.WorkingSet);
        PrivateMb = BytesToMb(row.PrivateBytes);
        DiskMbPerSecond = BytesToMb(row.ReadBps + row.WriteBps);
        ThreadCount = row.ThreadCount;
        HandleCount = row.HandleCount;
        HasDetailedCounts = detailedCountsAvailable;
        LastSeenMs = timestampMs > 0
            ? timestampMs
            : DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
        IsRunning = true;

        _samples.Enqueue(new ProcessLiveSample(
            LastSeenMs,
            CpuPercent,
            GpuPercent,
            WorkingSetMb,
            PrivateMb,
            DiskMbPerSecond));

        long cutoff = LastSeenMs - (long)TimeSpan.FromMinutes(10).TotalMilliseconds;
        while (_samples.Count > 1 && (_samples.Peek().TimestampMs < cutoff || _samples.Count > 600))
        {
            _samples.Dequeue();
        }

        UpdateOneMinuteSummary();
        SamplesChanged?.Invoke(this);
    }

    public void MarkEnded(long timestampMs)
    {
        IsRunning = false;
        LastSeenMs = timestampMs > 0
            ? timestampMs
            : DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
    }

    private void UpdateOneMinuteSummary()
    {
        if (_samples.Count == 0)
        {
            return;
        }

        long cutoff = LastSeenMs - (long)TimeSpan.FromMinutes(1).TotalMilliseconds;
        var recent = _samples.Where(sample => sample.TimestampMs >= cutoff).ToArray();
        if (recent.Length == 0)
        {
            return;
        }

        AverageCpuOneMinute = recent.Average(sample => sample.CpuPercent);
        PeakCpuOneMinute = recent.Max(sample => sample.CpuPercent);
        MemoryDeltaOneMinuteMb = WorkingSetMb - recent[0].WorkingSetMb;
        CpuDeltaOneMinute = CpuPercent - recent[0].CpuPercent;
    }

    private static double BytesToMb(ulong bytes) => bytes / (1024.0 * 1024.0);

    private static double Sanitize(double value) =>
        double.IsFinite(value) ? Math.Max(0, value) : 0;

    private static string FormatSigned(double value, string suffix)
    {
        if (Math.Abs(value) < 0.05)
        {
            return $"0{suffix}";
        }

        return string.Create(Inv, $"{(value > 0 ? "+" : string.Empty)}{value:F1}{suffix}");
    }
}
