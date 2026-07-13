using System.Globalization;
using Atlas.V0;

namespace Atlas.IpcClient;

/// <summary>
/// Pure mapping/formatting helpers for the M7 surface (privacy activity, startup
/// inventory, services). Free of I/O and of any WinUI type so the view-models
/// stay thin and the logic is unit-testable without a live server (task brief §1).
///
/// <para>
/// Tone matters here: the privacy strings are deliberately calm and factual —
/// they never imply malice (PRD §9.10.3). "In use now" and relative last-used
/// times describe access, not intent.
/// </para>
/// </summary>
public static class M7Formatter
{
    private static readonly CultureInfo Inv = CultureInfo.InvariantCulture;

    // ----------------------------------------------------------------------
    // Privacy (PRD §9.10).
    // ----------------------------------------------------------------------

    /// <summary>A capability's friendly display name (group header / label).</summary>
    public static string CapabilityLabel(CapabilityKind capability) => capability switch
    {
        CapabilityKind.Camera => "Camera",
        CapabilityKind.Microphone => "Microphone",
        CapabilityKind.Location => "Location",
        _ => "Other",
    };

    /// <summary>A Segoe Fluent glyph for a capability (for group headers / rows).</summary>
    public static string CapabilityGlyph(CapabilityKind capability) => capability switch
    {
        CapabilityKind.Camera => "",      // Camera / Video
        CapabilityKind.Microphone => "",  // Microphone
        CapabilityKind.Location => "",    // MapPin / Location
        _ => "",                          // Info
    };

    /// <summary>Packaged (Store/MSIX) vs. classic desktop indicator text.</summary>
    public static string PackagedLabel(bool packaged) => packaged ? "Packaged" : "Desktop app";

    /// <summary>
    /// A calm, factual usage-status line for one app+capability. When
    /// <paramref name="inUse"/> is true this is "In use now"; otherwise it is a
    /// relative "last used" phrase derived from <paramref name="lastStopMs"/>
    /// (falling back to <paramref name="lastStartMs"/>). No accusatory language.
    /// </summary>
    public static string UsageStatus(bool inUse, long lastStartMs, long lastStopMs, long nowMs)
    {
        if (inUse)
        {
            return "In use now";
        }

        // Prefer the stop time (when access ended); fall back to the start.
        long refMs = lastStopMs > 0 ? lastStopMs : lastStartMs;
        if (refMs <= 0)
        {
            return "Not used recently";
        }
        return "Last used " + RelativeTime(refMs, nowMs);
    }

    /// <summary>
    /// A compact relative-time phrase for a past instant: "just now", "5m ago",
    /// "2h ago", "3d ago". For future or non-positive inputs, returns "just now".
    /// Kept pure (takes <paramref name="nowMs"/>) so it is deterministically
    /// testable.
    /// </summary>
    public static string RelativeTime(long tsMs, long nowMs)
    {
        long deltaMs = nowMs - tsMs;
        if (deltaMs <= 0)
        {
            return "just now";
        }

        long seconds = deltaMs / 1000;
        if (seconds < 60)
        {
            return "just now";
        }
        long minutes = seconds / 60;
        if (minutes < 60)
        {
            return string.Format(Inv, "{0}m ago", minutes);
        }
        long hours = minutes / 60;
        if (hours < 24)
        {
            return string.Format(Inv, "{0}h ago", hours);
        }
        long days = hours / 24;
        return string.Format(Inv, "{0}d ago", days);
    }

    /// <summary>A one-line privacy event phrase, e.g. "Camera • Zoom started".</summary>
    public static string PrivacyEventLine(PrivacyEvent e)
    {
        var name = string.IsNullOrEmpty(e.DisplayName)
            ? (string.IsNullOrEmpty(e.AppId) ? "(unknown)" : e.AppId)
            : e.DisplayName;
        return string.Format(
            Inv,
            "{0} • {1} {2}",
            CapabilityLabel(e.Capability),
            name,
            e.Started ? "started" : "stopped");
    }

    // ----------------------------------------------------------------------
    // Startup inventory (PRD §9.8.1).
    // ----------------------------------------------------------------------

    /// <summary>A startup source's friendly group label.</summary>
    public static string StartupSourceLabel(StartupSource source) => source switch
    {
        StartupSource.RunKeyMachine => "Run keys (machine)",
        StartupSource.RunKeyUser => "Run keys (user)",
        StartupSource.StartupFolderMachine => "Startup folder (machine)",
        StartupSource.StartupFolderUser => "Startup folder (user)",
        StartupSource.ScheduledTask => "Scheduled tasks",
        StartupSource.Service => "Services",
        StartupSource.PackagedTask => "Packaged tasks",
        _ => "Other",
    };

    /// <summary>
    /// The coarse category a source belongs to, used to group the startup table
    /// (Run keys / Startup folders / Tasks / Services / Packaged).
    /// </summary>
    public static string StartupCategory(StartupSource source) => source switch
    {
        StartupSource.RunKeyMachine or StartupSource.RunKeyUser => "Run keys",
        StartupSource.StartupFolderMachine or StartupSource.StartupFolderUser => "Startup folders",
        StartupSource.ScheduledTask => "Tasks",
        StartupSource.Service => "Services",
        StartupSource.PackagedTask => "Packaged",
        _ => "Other",
    };

    /// <summary>Enabled/disabled pill caption.</summary>
    public static string EnabledLabel(bool enabled) => enabled ? "Enabled" : "Disabled";

    // ----------------------------------------------------------------------
    // Services (PRD §9.9.1).
    // ----------------------------------------------------------------------

    /// <summary>A service state's friendly label.</summary>
    public static string ServiceStateLabel(ServiceState state) => state switch
    {
        ServiceState.ServiceStopped => "Stopped",
        ServiceState.ServiceStartPending => "Starting",
        ServiceState.ServiceStopPending => "Stopping",
        ServiceState.ServiceRunning => "Running",
        ServiceState.ServiceContinuePending => "Continuing",
        ServiceState.ServicePausePending => "Pausing",
        ServiceState.ServicePaused => "Paused",
        _ => "Unknown",
    };

    /// <summary>
    /// A neutral severity token for a service state, so the UI can pick a color
    /// without embedding policy in XAML: "running" (positive), "transitional"
    /// (pending states), "stopped" (neutral/inactive), "unknown".
    /// </summary>
    public static string ServiceStateSeverity(ServiceState state) => state switch
    {
        ServiceState.ServiceRunning => "running",
        ServiceState.ServiceStartPending
            or ServiceState.ServiceStopPending
            or ServiceState.ServiceContinuePending
            or ServiceState.ServicePausePending => "transitional",
        ServiceState.ServicePaused => "transitional",
        ServiceState.ServiceStopped => "stopped",
        _ => "unknown",
    };

    /// <summary>A service start-type's friendly label.</summary>
    public static string ServiceStartTypeLabel(ServiceStartType startType) => startType switch
    {
        ServiceStartType.StartBoot => "Boot",
        ServiceStartType.StartSystem => "System",
        ServiceStartType.StartAuto => "Automatic",
        ServiceStartType.StartManual => "Manual",
        ServiceStartType.StartDisabled => "Disabled",
        _ => "Unknown",
    };

    /// <summary>
    /// A friendly start-type label that folds in the delayed-auto-start flag,
    /// e.g. "Automatic (delayed)".
    /// </summary>
    public static string ServiceStartTypeLabel(ServiceStartType startType, bool delayedAutoStart)
    {
        var label = ServiceStartTypeLabel(startType);
        return delayedAutoStart && startType == ServiceStartType.StartAuto
            ? label + " (delayed)"
            : label;
    }

    /// <summary>PID display: blank when 0 (not running), else the number.</summary>
    public static string PidText(uint pid) =>
        pid == 0 ? string.Empty : pid.ToString(Inv);

    /// <summary>
    /// Truncates a (possibly long) command / path to <paramref name="max"/>
    /// characters with an ellipsis, for a table cell. The full value belongs in
    /// a tooltip. Returns the original when short enough or when max &lt;= 0.
    /// </summary>
    public static string Truncate(string? value, int max = 80)
    {
        var s = value ?? string.Empty;
        if (max <= 0 || s.Length <= max)
        {
            return s;
        }
        return s.Substring(0, max - 1) + "…";
    }
}
