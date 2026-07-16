using System.Globalization;
using Atlas.App.Services;
using CommunityToolkit.Mvvm.ComponentModel;

namespace Atlas.App.ViewModels;

/// <summary>
/// One entry in the Overview "Top consumers" list: name, PID, CPU%, working
/// set. Observable so the short list updates in place each tick without
/// rebuilding. Pre-formatted display strings so the XAML uses plain
/// <c>x:Bind</c> without a converter.
/// </summary>
public sealed partial class ConsumerRowViewModel : ObservableObject
{
    private static readonly CultureInfo Inv = CultureInfo.InvariantCulture;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(AutomationName))]
    private string _imageName = string.Empty;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(PidText))]
    [NotifyPropertyChangedFor(nameof(AutomationName))]
    private uint _pid;

    [ObservableProperty] private long _createTime100ns;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(CpuText))]
    [NotifyPropertyChangedFor(nameof(AutomationName))]
    private double _cpuPercent;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(WorkingSetText))]
    [NotifyPropertyChangedFor(nameof(AutomationName))]
    private double _workingSetMb;

    public string CpuText => CpuPercent.ToString("F1", Inv) + " %";
    public string WorkingSetText => WorkingSetMb.ToString("F0", Inv) + " MB";
    public string PidText => "PID " + Pid.ToString(Inv);
    public string AutomationName =>
        $"{ImageName}, {PidText}, CPU {CpuText}, memory {WorkingSetText}. Open process inspector.";

    public void Update(MetricsRow row)
    {
        ImageName = row.ImageName;
        Pid = row.Pid;
        CreateTime100ns = row.CreateTime100ns;
        CpuPercent = row.CpuPercent;
        WorkingSetMb = row.WorkingSet / (1024.0 * 1024.0);
    }
}
