using System.Globalization;
using CommunityToolkit.Mvvm.ComponentModel;

namespace Atlas.App.ViewModels;

/// <summary>
/// One row in the Live Activity table. Mutable/observable so the collection is
/// updated in place each tick (never rebuilt), keeping virtualization and
/// selection stable. Identity is (PID, CreateTime) — matching the service's
/// process identity (proto <c>create_time_100ns</c>).
///
/// Numeric values are exposed both raw (for sorting/inspection) and as
/// pre-formatted display strings so the XAML uses simple <c>x:Bind</c> without
/// a value converter.
/// </summary>
public sealed partial class ProcessRowViewModel : ObservableObject
{
    private static readonly CultureInfo Inv = CultureInfo.InvariantCulture;

    public uint Pid { get; }
    public long CreateTime100ns { get; }

    [ObservableProperty] private string _imageName = string.Empty;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(CpuText))]
    private double _cpuPercent;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(WorkingSetText))]
    private double _workingSetMb;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(PrivateText))]
    private double _privateMb;

    [ObservableProperty] private uint _threadCount;
    [ObservableProperty] private uint _handleCount;

    public string CpuText => CpuPercent.ToString("F1", Inv);
    public string WorkingSetText => WorkingSetMb.ToString("F1", Inv);
    public string PrivateText => PrivateMb.ToString("F1", Inv);

    public ProcessRowViewModel(uint pid, long createTime100ns)
    {
        Pid = pid;
        CreateTime100ns = createTime100ns;
    }

    /// <summary>Updates the mutable fields from a fresh proto row.</summary>
    public void Update(Atlas.V0.ProcessRow row)
    {
        ImageName = row.ImageName;
        CpuPercent = row.CpuPermille / 10.0;
        WorkingSetMb = row.WorkingSet / (1024.0 * 1024.0);
        PrivateMb = row.PrivateBytes / (1024.0 * 1024.0);
        ThreadCount = row.ThreadCount;
        HandleCount = row.HandleCount;
    }
}
