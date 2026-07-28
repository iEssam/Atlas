using System.Globalization;
using Atlas.V0;

namespace Atlas.IpcClient;

/// <summary>
/// Pure mapping/formatting helpers for the R3 forensics surface — system changes
/// (PRD §9.13) and reliability/crash records (PRD §9.14). Free of I/O and of any
/// WinUI type so the view-models stay thin and the logic is unit-testable without a
/// live server (task brief §1).
///
/// <para>
/// Tone is the whole point here. A <b>system change</b> is information — "what
/// changed?" — never a threat, so its category colors are deliberately calm
/// (blue/green/neutral) and never borrow the red danger palette. A <b>crash
/// record</b> is history plus correlated context, never an accusation: its color
/// tokens top out at <em>caution</em> (amber), never <em>critical</em> (red), and
/// its context lines are hedged ("around this time", "may be related") because
/// correlation is not blame (PRD §9.14; mirrors the Diagnostics epistemic tone).
/// </para>
/// </summary>
public static class R3Formatter
{
    private static readonly CultureInfo Inv = CultureInfo.InvariantCulture;

    // ----------------------------------------------------------------------
    // System changes (PRD §9.13).
    // ----------------------------------------------------------------------

    /// <summary>A friendly display label for a system-change kind.</summary>
    public static string SystemChangeKindLabel(SystemChangeKind kind) => kind switch
    {
        SystemChangeKind.AppInstalled => "App installed",
        SystemChangeKind.AppUpdated => "App updated",
        SystemChangeKind.AppRemoved => "App removed",
        SystemChangeKind.DriverInstalled => "Driver installed",
        SystemChangeKind.DriverUpdated => "Driver updated",
        SystemChangeKind.WindowsUpdate => "Windows update",
        SystemChangeKind.ServiceInstalled => "Service installed",
        SystemChangeKind.ServiceConfigChanged => "Service changed",
        SystemChangeKind.ServiceRemoved => "Service removed",
        SystemChangeKind.StartupAdded => "Startup item added",
        SystemChangeKind.StartupRemoved => "Startup item removed",
        SystemChangeKind.ScheduledTaskAdded => "Scheduled task added",
        SystemChangeKind.ScheduledTaskRemoved => "Scheduled task removed",
        SystemChangeKind.PowerPlanChanged => "Power plan changed",
        SystemChangeKind.DefaultAppChanged => "Default app changed",
        _ => "System change",
    };

    /// <summary>A Segoe Fluent glyph for a system-change kind (list-row leading icon).</summary>
    public static string SystemChangeKindGlyph(SystemChangeKind kind) => kind switch
    {
        SystemChangeKind.AppInstalled => "",          // Add
        SystemChangeKind.AppUpdated => "",            // UpdateRestore
        SystemChangeKind.AppRemoved => "",            // Remove
        SystemChangeKind.DriverInstalled => "",       // Repair
        SystemChangeKind.DriverUpdated => "",         // Repair
        SystemChangeKind.WindowsUpdate => "",         // Sync
        SystemChangeKind.ServiceInstalled => "",      // Processing
        SystemChangeKind.ServiceConfigChanged => "",  // Processing
        SystemChangeKind.ServiceRemoved => "",        // Processing
        SystemChangeKind.StartupAdded => "",          // PowerButton (launch-at-start)
        SystemChangeKind.StartupRemoved => "",        // PowerButton
        SystemChangeKind.ScheduledTaskAdded => "",    // History / clock
        SystemChangeKind.ScheduledTaskRemoved => "",  // History / clock
        SystemChangeKind.PowerPlanChanged => "",      // Battery / power
        SystemChangeKind.DefaultAppChanged => "",     // AllApps
        _ => "",                                       // Info
    };

    /// <summary>
    /// A <b>calm, non-alarmist</b> category color token for a system-change kind:
    /// one of "install" / "update" / "remove" / "driver" / "service" / "startup" /
    /// "task" / "power" / "default". The consumer maps these to muted blue / green /
    /// neutral brushes — <em>never</em> the red danger palette. A change is
    /// information about what happened, not a warning (task brief; PRD §9.13).
    /// </summary>
    public static string SystemChangeCategoryToken(SystemChangeKind kind) => kind switch
    {
        SystemChangeKind.AppInstalled => "install",
        SystemChangeKind.AppUpdated => "update",
        SystemChangeKind.AppRemoved => "remove",
        SystemChangeKind.DriverInstalled => "driver",
        SystemChangeKind.DriverUpdated => "driver",
        SystemChangeKind.WindowsUpdate => "update",
        SystemChangeKind.ServiceInstalled => "service",
        SystemChangeKind.ServiceConfigChanged => "service",
        SystemChangeKind.ServiceRemoved => "remove",
        SystemChangeKind.StartupAdded => "startup",
        SystemChangeKind.StartupRemoved => "remove",
        SystemChangeKind.ScheduledTaskAdded => "task",
        SystemChangeKind.ScheduledTaskRemoved => "remove",
        SystemChangeKind.PowerPlanChanged => "power",
        SystemChangeKind.DefaultAppChanged => "default",
        _ => "default",
    };

    /// <summary>
    /// A compact one-line summary of a change: the kind label plus its subject,
    /// e.g. "App installed: Zoom". Falls back to just the kind label when no subject
    /// is known. Suitable for a timeline marker or a collapsed row.
    /// </summary>
    public static string ChangeSummary(SystemChangeKind kind, string? subject)
    {
        var label = SystemChangeKindLabel(kind);
        var s = (subject ?? string.Empty).Trim();
        return s.Length == 0 ? label : string.Format(Inv, "{0}: {1}", label, s);
    }

    /// <summary>
    /// A calm provenance line tying a change to who shipped it and what applied it,
    /// e.g. "Zoom Video Communications • via msiexec.exe". Either part may be empty;
    /// returns an empty string when neither is known. Purely factual — "via" names
    /// the installer that made the change, it does not imply intent.
    /// </summary>
    public static string ChangeProvenance(string? publisher, string? responsible)
    {
        var pub = (publisher ?? string.Empty).Trim();
        var resp = (responsible ?? string.Empty).Trim();
        if (pub.Length == 0 && resp.Length == 0)
        {
            return string.Empty;
        }
        if (resp.Length == 0)
        {
            return pub;
        }
        var via = string.Format(Inv, "via {0}", resp);
        return pub.Length == 0 ? via : string.Format(Inv, "{0} • {1}", pub, via);
    }

    /// <summary>
    /// A short pill caption for the informational reversibility flag: "Reversible"
    /// when Atlas knows a reversal path, empty otherwise. Informational only this
    /// milestone (proto R3) — it never promises an action.
    /// </summary>
    public static string ReversibleLabel(bool reversible) => reversible ? "Reversible" : string.Empty;

    // ----------------------------------------------------------------------
    // Reliability / crashes (PRD §9.14).
    // ----------------------------------------------------------------------

    /// <summary>A friendly display label for a crash-record kind.</summary>
    public static string CrashKindLabel(CrashKind kind) => kind switch
    {
        CrashKind.AppCrash => "App crash",
        CrashKind.AppHang => "App stopped responding",
        CrashKind.Bugcheck => "System bugcheck",
        CrashKind.ServiceFailure => "Service failure",
        CrashKind.UnexpectedShutdown => "Unexpected shutdown",
        CrashKind.GpuDriverReset => "GPU driver reset",
        _ => "Reliability event",
    };

    /// <summary>A Segoe Fluent glyph for a crash-record kind (list-row leading icon).</summary>
    public static string CrashKindGlyph(CrashKind kind) => kind switch
    {
        CrashKind.AppCrash => "",          // Bug
        CrashKind.AppHang => "",           // Pause (stopped responding)
        CrashKind.Bugcheck => "",          // Warning (stop error)
        CrashKind.ServiceFailure => "",    // Processing (service)
        CrashKind.UnexpectedShutdown => "",// PowerButton
        _ => "",                            // Info
    };

    /// <summary>
    /// A <b>caution-level</b> (never danger) color token for a crash-record kind:
    /// "caution" for the recorded faults, "neutral" for an unexpected shutdown (a
    /// power event, not a fault). The consumer maps "caution" to the amber caution
    /// brush and "neutral" to muted secondary text — the red <em>critical</em>
    /// brush is deliberately never used, because a crash record is history to
    /// understand, not an alarm to sound (task brief; PRD §9.14).
    /// </summary>
    public static string CrashCautionToken(CrashKind kind) => kind switch
    {
        CrashKind.AppCrash => "caution",
        CrashKind.AppHang => "caution",
        CrashKind.Bugcheck => "caution",
        CrashKind.ServiceFailure => "caution",
        CrashKind.UnexpectedShutdown => "neutral",
        CrashKind.GpuDriverReset => "caution",
        _ => "neutral",
    };

    /// <summary>
    /// The crash subject, falling back to the kind label when the record names no
    /// specific subject (so a row never renders blank).
    /// </summary>
    public static string CrashSubjectText(CrashKind kind, string? subject)
    {
        var s = (subject ?? string.Empty).Trim();
        return s.Length == 0 ? CrashKindLabel(kind) : s;
    }

    /// <summary>
    /// A compact fault descriptor for a crash record, joining the faulting module /
    /// bugcheck string and the exception code where known, e.g.
    /// "ntdll.dll • 0xC0000005". Either part may be empty; returns an empty string
    /// when neither is known (the record's subject and context still stand alone).
    /// </summary>
    public static string CrashFaultLine(string? fault, string? exceptionCode)
    {
        var f = (fault ?? string.Empty).Trim();
        var c = (exceptionCode ?? string.Empty).Trim();
        if (f.Length == 0 && c.Length == 0)
        {
            return string.Empty;
        }
        if (c.Length == 0)
        {
            return f;
        }
        return f.Length == 0 ? c : string.Format(Inv, "{0} • {1}", f, c);
    }

    /// <summary>
    /// Normalizes one correlated-context line for display: trims surrounding
    /// whitespace and supplies a calm fallback for an empty entry. The context text
    /// itself is assembled and confidence-tagged server-side (peak memory before, a
    /// recent update, a repeated-restart note); the UI presents it verbatim under a
    /// hedged heading rather than restating it as a cause (correlation, not blame).
    /// </summary>
    public static string CrashContextLine(string? raw)
    {
        var s = (raw ?? string.Empty).Trim();
        return s.Length == 0 ? "(context noted)" : s;
    }

    /// <summary>
    /// The standing, hedged heading shown above a crash's correlated-context list.
    /// Kept here so the framing stays consistent and reviewable in one place: these
    /// are things Atlas noticed <em>around</em> the event, not a verdict on its cause.
    /// </summary>
    public const string ContextIntro =
        "What Atlas noticed around this time — this is context, not a cause. It may or may not be related.";
}
