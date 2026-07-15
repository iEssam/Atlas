using System.Globalization;
using Atlas.V0;

namespace Atlas.IpcClient;

/// <summary>
/// Pure mapping/formatting helpers for the remote support-bundle export surface
/// (PRD §9.18, §18.3). Free of I/O and of any WinUI type so the dialog stays thin
/// and the logic is unit-testable without a live server (task brief §1).
///
/// <para>
/// A support bundle is one redacted, self-contained diagnostic document a user
/// hands to an IT/support engineer. The tone here is calm and honest: the bundle
/// is <b>diagnostic evidence for a support engineer</b>, and redaction (on by
/// default) removes the selected personal details before the file is created, so
/// unrelated personal activity never leaves the machine. These helpers supply the
/// plain-English section labels/descriptions and the "what was stripped" summary
/// the dialog shows so the user can see exactly what the bundle contains.
/// </para>
/// </summary>
public static class SupportBundleFormatter
{
    private static readonly CultureInfo Inv = CultureInfo.InvariantCulture;

    /// <summary>
    /// The bundle sections in the order the dialog presents them. Excludes the
    /// proto's <c>UNSPECIFIED</c> zero value (not a real, selectable section).
    /// </summary>
    public static readonly IReadOnlyList<SupportBundleSection> AllSections = new[]
    {
        SupportBundleSection.BundleDeviceInfo,
        SupportBundleSection.BundleHealth,
        SupportBundleSection.BundleIncidents,
        SupportBundleSection.BundleSystemChanges,
        SupportBundleSection.BundleCrashes,
        SupportBundleSection.BundleServices,
        SupportBundleSection.BundleStartup,
        SupportBundleSection.BundleSelfMetrics,
    };

    /// <summary>The set of sections whose contents are bounded by the chosen time window.</summary>
    public static readonly IReadOnlyList<SupportBundleSection> WindowedSections = new[]
    {
        SupportBundleSection.BundleIncidents,
        SupportBundleSection.BundleSystemChanges,
        SupportBundleSection.BundleCrashes,
    };

    // ----------------------------------------------------------------------
    // Sections (PRD §9.18) — friendly label + a brief plain-English note.
    // ----------------------------------------------------------------------

    /// <summary>A friendly display label for a bundle section.</summary>
    public static string SectionLabel(SupportBundleSection section) => section switch
    {
        SupportBundleSection.BundleDeviceInfo => "Device info",
        SupportBundleSection.BundleHealth => "Health",
        SupportBundleSection.BundleIncidents => "Incidents & diagnoses",
        SupportBundleSection.BundleSystemChanges => "System changes",
        SupportBundleSection.BundleCrashes => "Crashes",
        SupportBundleSection.BundleServices => "Services",
        SupportBundleSection.BundleStartup => "Startup",
        SupportBundleSection.BundleSelfMetrics => "Atlas overhead",
        _ => "Section",
    };

    /// <summary>A one-line, plain-English description of what a section includes.</summary>
    public static string SectionDescription(SupportBundleSection section) => section switch
    {
        SupportBundleSection.BundleDeviceInfo =>
            "Basic hardware and OS details: model, CPU, memory, and Windows version.",
        SupportBundleSection.BundleHealth =>
            "Recent system health — CPU, memory, and disk trends.",
        SupportBundleSection.BundleIncidents =>
            "Detected slowdowns and Atlas's diagnosis of what drove each one.",
        SupportBundleSection.BundleSystemChanges =>
            "What changed recently: installs, updates, drivers, services, and startup items.",
        SupportBundleSection.BundleCrashes =>
            "Recent crashes, hangs, and unexpected shutdowns with their context.",
        SupportBundleSection.BundleServices =>
            "The Windows service inventory and each service's current state.",
        SupportBundleSection.BundleStartup =>
            "Programs and tasks configured to run at sign-in or boot.",
        SupportBundleSection.BundleSelfMetrics =>
            "Atlas's own resource use, so support can see the tool's own footprint.",
        _ => string.Empty,
    };

    /// <summary>A Segoe Fluent glyph for a bundle section (checkbox leading icon).</summary>
    public static string SectionGlyph(SupportBundleSection section) => section switch
    {
        SupportBundleSection.BundleDeviceInfo => "",   // Devices
        SupportBundleSection.BundleHealth => "",       // Health / heartbeat
        SupportBundleSection.BundleIncidents => "",    // Warning
        SupportBundleSection.BundleSystemChanges => "", // Map / changes
        SupportBundleSection.BundleCrashes => "",      // Bug / error
        SupportBundleSection.BundleServices => "",     // Processing
        SupportBundleSection.BundleStartup => "",      // Boot / power
        SupportBundleSection.BundleSelfMetrics => "",  // Diagnostic
        _ => "",                                       // Page
    };

    /// <summary>True when a section's contents depend on the chosen time window.</summary>
    public static bool IsWindowed(SupportBundleSection section) =>
        section == SupportBundleSection.BundleIncidents ||
        section == SupportBundleSection.BundleSystemChanges ||
        section == SupportBundleSection.BundleCrashes;

    // ----------------------------------------------------------------------
    // Time window (incidents / changes / crashes). Offered as fixed spans.
    // ----------------------------------------------------------------------

    /// <summary>A friendly label for a lookback window given its length in hours.</summary>
    public static string WindowLabel(int hours) => hours switch
    {
        24 => "Last 24 hours",
        72 => "Last 72 hours",
        168 => "Last 7 days",
        _ => hours % 24 == 0
            ? string.Format(Inv, "Last {0} days", hours / 24)
            : string.Format(Inv, "Last {0} hours", hours),
    };

    /// <summary>
    /// The window start (epoch ms) for a lookback of <paramref name="hours"/> ending
    /// at <paramref name="nowMs"/>. Clamped at 0 so an absurdly large span never
    /// produces a negative bound.
    /// </summary>
    public static long WindowFromMs(long nowMs, int hours)
    {
        var span = (long)hours * 3_600_000L;
        var from = nowMs - span;
        return from < 0 ? 0 : from;
    }

    // ----------------------------------------------------------------------
    // Redaction — the safety framing the user sees before generating, and the
    // echo of what the server actually stripped after.
    // ----------------------------------------------------------------------

    /// <summary>
    /// A plain-English summary of what a redaction selection will strip before the
    /// bundle is created, for the export dialog so the user knows before generating.
    /// Frames redaction as protecting unrelated personal activity, since this file
    /// is meant to leave the machine to a support engineer (PRD §9.18). Returns a
    /// clear warning when nothing is selected.
    /// </summary>
    public static string RedactionSummary(RedactionOptions options)
    {
        var parts = SelectedCategories(options);
        if (parts.Count == 0)
        {
            return "Nothing will be removed — this bundle will include user names, " +
                   "the computer name, file paths, and command lines in full.";
        }
        return "Before the file is created, Atlas will remove: " +
               JoinCategories(parts) + ".";
    }

    /// <summary>
    /// A summary of the redaction categories the server reported it actually applied
    /// (the reply's <c>redaction_applied</c> echo), shown back to the user so they
    /// can see what was stripped. Returns a plain note when the list is empty (no
    /// redaction was applied — the bundle includes all details).
    /// </summary>
    public static string RedactionAppliedSummary(IEnumerable<string>? applied)
    {
        var list = (applied ?? Array.Empty<string>())
            .Select(a => a?.Trim() ?? string.Empty)
            .Where(a => a.Length > 0)
            .ToList();
        if (list.Count == 0)
        {
            return "No redaction was applied — this bundle includes all details.";
        }
        return "Removed before this file was created: " + JoinCategories(list) + ".";
    }

    /// <summary>The friendly redaction-category names selected in the given options, in a stable order.</summary>
    private static List<string> SelectedCategories(RedactionOptions options)
    {
        var parts = new List<string>();
        if (options is null)
        {
            return parts;
        }
        if (options.RedactUserNames)
        {
            parts.Add("user names");
        }
        if (options.RedactComputerName)
        {
            parts.Add("the computer name");
        }
        if (options.RedactPaths)
        {
            parts.Add("file paths");
        }
        if (options.RedactCommandLines)
        {
            parts.Add("command lines");
        }
        return parts;
    }

    /// <summary>Joins category phrases with commas and a trailing "and" for the last item.</summary>
    private static string JoinCategories(IReadOnlyList<string> parts)
    {
        if (parts.Count == 0)
        {
            return string.Empty;
        }
        if (parts.Count == 1)
        {
            return parts[0];
        }
        if (parts.Count == 2)
        {
            return parts[0] + " and " + parts[1];
        }
        return string.Join(", ", parts.Take(parts.Count - 1)) + ", and " + parts[^1];
    }
}
