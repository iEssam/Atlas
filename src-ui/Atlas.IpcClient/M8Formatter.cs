using System.Globalization;
using Atlas.V0;

namespace Atlas.IpcClient;

/// <summary>
/// Pure mapping/formatting helpers for the M8 diagnostics surface (incidents,
/// diagnoses, reports). Free of I/O and of any WinUI type so the view-models stay
/// thin and the logic is unit-testable without a live server (task brief §1).
///
/// <para>
/// Tone matters more here than anywhere else in the app. A diagnosis states what
/// was measured and how sure the engine is — never more (PRD §9.15.2, §9.16.4).
/// The confidence ladder therefore maps to <b>calm, epistemic</b> color tokens:
/// "low confidence" is honesty about evidence, not danger, so it must never be
/// painted the alarming red reserved for a genuinely critical incident. Only the
/// <em>incident severity</em> scale carries the danger colors.
/// </para>
/// </summary>
public static class M8Formatter
{
    private static readonly CultureInfo Inv = CultureInfo.InvariantCulture;

    // ----------------------------------------------------------------------
    // Incident kind (PRD §9.15.1).
    // ----------------------------------------------------------------------

    /// <summary>A friendly display label for an incident kind.</summary>
    public static string IncidentKindLabel(IncidentKind kind) => kind switch
    {
        IncidentKind.CpuSaturation => "CPU saturation",
        IncidentKind.MemoryPressure => "Memory pressure",
        IncidentKind.DiskLatency => "Disk latency",
        IncidentKind.GpuSaturation => "GPU saturation",
        IncidentKind.GpuMemoryExhaustion => "GPU memory pressure",
        IncidentKind.GpuThermalThrottling => "GPU thermal throttling",
        _ => "Incident",
    };

    /// <summary>A Segoe Fluent glyph for an incident kind (list-row leading icon).</summary>
    public static string IncidentKindGlyph(IncidentKind kind) => kind switch
    {
        IncidentKind.CpuSaturation => "",   // Diagnostic / activity
        IncidentKind.MemoryPressure => "",  // Chip / board
        IncidentKind.DiskLatency => "",     // HardDrive
        _ => "",                            // Warning
    };

    /// <summary>The short resource noun a kind concerns, for attribution phrasing.</summary>
    public static string ResourceNoun(IncidentKind kind) => kind switch
    {
        IncidentKind.CpuSaturation => "CPU",
        IncidentKind.MemoryPressure => "memory",
        IncidentKind.DiskLatency => "disk",
        IncidentKind.GpuSaturation => "GPU",
        IncidentKind.GpuMemoryExhaustion => "GPU memory",
        IncidentKind.GpuThermalThrottling => "GPU thermal state",
        _ => "the resource",
    };

    /// <summary>
    /// A plain-language peak line for the driving metric of an incident. CPU and
    /// memory peaks are permille (→ percent); disk-latency peaks are milliseconds
    /// (PRD: <c>peak_value</c> is "permille or ms"). Empty when there is nothing
    /// meaningful to show.
    /// </summary>
    public static string PeakValueText(IncidentKind kind, double peakValue)
    {
        switch (kind)
        {
            case IncidentKind.CpuSaturation:
                return string.Format(Inv, "Peaked at {0:0.#}% CPU", peakValue / 10.0);
            case IncidentKind.MemoryPressure:
                return string.Format(Inv, "Peaked at {0:0.#}% memory", peakValue / 10.0);
            case IncidentKind.DiskLatency:
                return string.Format(Inv, "Peaked at {0:0.#} ms latency", peakValue);
            case IncidentKind.GpuSaturation:
                return string.Format(Inv, "Peaked at {0:0.#}% GPU", peakValue);
            case IncidentKind.GpuMemoryExhaustion:
                return string.Format(Inv, "Peaked at {0:0.#}% of the GPU memory budget", peakValue);
            case IncidentKind.GpuThermalThrottling:
                return "Hardware reported thermal throttling";
            default:
                return string.Empty;
        }
    }

    // ----------------------------------------------------------------------
    // Severity (PRD §9.15.1) — this is the DANGER scale, so it may use the
    // caution/critical colors. Distinct from the confidence scale below.
    // ----------------------------------------------------------------------

    /// <summary>A friendly label for an incident severity.</summary>
    public static string SeverityLabel(Severity severity) => severity switch
    {
        Severity.Info => "Info",
        Severity.Warning => "Warning",
        Severity.Critical => "Critical",
        _ => "Unknown",
    };

    /// <summary>
    /// A neutral severity token so XAML can pick a color without embedding policy:
    /// "info" / "warning" / "critical" / "unknown". These may map to the caution
    /// and critical brushes — severity is the one place danger colors belong.
    /// </summary>
    public static string SeverityColorToken(Severity severity) => severity switch
    {
        Severity.Info => "info",
        Severity.Warning => "warning",
        Severity.Critical => "critical",
        _ => "unknown",
    };

    // ----------------------------------------------------------------------
    // Confidence (PRD §9.15.2) — the epistemic ladder. Labels and CALM tokens.
    // ----------------------------------------------------------------------

    /// <summary>
    /// A friendly confidence label. "Confirmed" stands alone (a measured fact);
    /// the graded rungs read as "… confidence"; the bottom rung is stated as
    /// "Insufficient evidence" so it never masquerades as a weak conclusion.
    /// </summary>
    public static string ConfidenceLabel(Confidence confidence) => confidence switch
    {
        Confidence.Confirmed => "Confirmed",
        Confidence.High => "High confidence",
        Confidence.Medium => "Medium confidence",
        Confidence.Low => "Low confidence",
        Confidence.Insufficient => "Insufficient evidence",
        _ => "Unknown",
    };

    /// <summary>
    /// A <b>calm, epistemic</b> color token for a confidence rung:
    /// "confirmed" / "high" / "medium" / "low" / "insufficient" / "unknown".
    /// The consumer maps these to muted, non-alarming brushes — deliberately NOT
    /// the red danger palette. Low confidence is honesty about evidence, and the
    /// UI must never make honesty look like an emergency (task brief §1).
    /// </summary>
    public static string ConfidenceColorToken(Confidence confidence) => confidence switch
    {
        Confidence.Confirmed => "confirmed",
        Confidence.High => "high",
        Confidence.Medium => "medium",
        Confidence.Low => "low",
        Confidence.Insufficient => "insufficient",
        _ => "unknown",
    };

    // ----------------------------------------------------------------------
    // Attribution (share of the saturated resource, 0..1).
    // ----------------------------------------------------------------------

    /// <summary>
    /// A rounded share phrase for an attribution in 0..1, e.g. 0.723 → "~72%".
    /// Empty for a non-positive attribution (the factor isn't quantified).
    /// </summary>
    public static string AttributionShare(double attribution)
    {
        if (attribution <= 0)
        {
            return string.Empty;
        }
        var clamped = Math.Clamp(attribution, 0.0, 1.0);
        var pct = (int)Math.Round(clamped * 100.0, MidpointRounding.AwayFromZero);
        return string.Format(Inv, "~{0}%", pct);
    }

    /// <summary>
    /// A full attribution phrase tying the share to the resource, e.g.
    /// "~72% of CPU". Empty when the attribution is not quantified.
    /// </summary>
    public static string AttributionText(double attribution, IncidentKind kind)
    {
        var share = AttributionShare(attribution);
        return share.Length == 0
            ? string.Empty
            : string.Format(Inv, "{0} of {1}", share, ResourceNoun(kind));
    }

    /// <summary>
    /// A one-line process descriptor for a contributing factor, e.g.
    /// "chrome.exe (pid 4242)". Falls back to just the pid, or empty when the
    /// factor isn't process-specific.
    /// </summary>
    public static string ProcessText(string? imageName, uint pid)
    {
        var hasName = !string.IsNullOrWhiteSpace(imageName);
        if (hasName && pid > 0)
        {
            return string.Format(Inv, "{0} (pid {1})", imageName, pid);
        }
        if (hasName)
        {
            return imageName!;
        }
        return pid > 0 ? string.Format(Inv, "pid {0}", pid) : string.Empty;
    }

    // ----------------------------------------------------------------------
    // Time windows / durations.
    // ----------------------------------------------------------------------

    /// <summary>
    /// A compact human duration for a span in milliseconds: "45s", "5m", "1h 3m",
    /// "2d 4h". Sub-second and non-positive spans render as "0s". Kept coarse (at
    /// most two units) so it reads at a glance.
    /// </summary>
    public static string DurationText(long ms)
    {
        if (ms <= 0)
        {
            return "0s";
        }
        long seconds = ms / 1000;
        if (seconds < 60)
        {
            return string.Format(Inv, "{0}s", seconds);
        }
        long minutes = seconds / 60;
        if (minutes < 60)
        {
            long remSec = seconds % 60;
            return remSec > 0
                ? string.Format(Inv, "{0}m {1}s", minutes, remSec)
                : string.Format(Inv, "{0}m", minutes);
        }
        long hours = minutes / 60;
        if (hours < 24)
        {
            long remMin = minutes % 60;
            return remMin > 0
                ? string.Format(Inv, "{0}h {1}m", hours, remMin)
                : string.Format(Inv, "{0}h", hours);
        }
        long days = hours / 24;
        long remHr = hours % 24;
        return remHr > 0
            ? string.Format(Inv, "{0}d {1}h", days, remHr)
            : string.Format(Inv, "{0}d", days);
    }

    /// <summary>
    /// A one-line incident window phrase built from the start, end (0 = still
    /// ongoing) and "now": e.g. "Started 2h ago • lasted 5m", or for an open
    /// incident "Started 2h ago • ongoing". Reuses the shared relative-time
    /// bucketing so phrasing stays consistent with the rest of the app.
    /// </summary>
    public static string IncidentWindowText(long startMs, long endMs, long nowMs)
    {
        var started = "Started " + M7Formatter.RelativeTime(startMs, nowMs);
        if (endMs <= 0)
        {
            return started + " • ongoing";
        }
        var span = endMs - startMs;
        return started + " • lasted " + DurationText(span);
    }

    /// <summary>
    /// A precise "from – to" window label (local time) for the diagnosis header,
    /// e.g. "14:32 – 14:37 (5m)". An open window ends at "now". Times are rendered
    /// from the supplied <paramref name="toLocal"/> conversion so the helper stays
    /// pure and testable; callers pass a formatter that localizes epoch ms.
    /// </summary>
    public static string WindowRangeText(long startMs, long endMs, long nowMs, Func<long, string> toLocal)
    {
        long effectiveEnd = endMs <= 0 ? nowMs : endMs;
        var span = effectiveEnd - startMs;
        var tail = endMs <= 0 ? " (ongoing)" : string.Format(Inv, " ({0})", DurationText(span));
        return string.Format(Inv, "{0} – {1}{2}", toLocal(startMs), toLocal(effectiveEnd), tail);
    }

    // ----------------------------------------------------------------------
    // Evidence.
    // ----------------------------------------------------------------------

    /// <summary>
    /// A compact "metric = value" tag for an evidence item, e.g. "sys.cpu = 97.3".
    /// Empty when the item carries no metric (its <see cref="EvidenceItem.Text"/>
    /// already stands alone). Values are formatted to one decimal.
    /// </summary>
    public static string EvidenceMetricText(string? metric, double value)
    {
        if (string.IsNullOrWhiteSpace(metric))
        {
            return string.Empty;
        }
        return string.Format(Inv, "{0} = {1:0.#}", metric, value);
    }

    // ----------------------------------------------------------------------
    // Reports (PRD §9.18).
    // ----------------------------------------------------------------------

    /// <summary>A friendly label for a report format.</summary>
    public static string ReportFormatLabel(ReportFormat format) => format switch
    {
        ReportFormat.ReportHtml => "HTML",
        ReportFormat.ReportCsv => "CSV",
        ReportFormat.ReportJson => "JSON",
        ReportFormat.ReportText => "Plain text",
        _ => "Unknown",
    };

    /// <summary>The conventional file extension (no dot) for a report format.</summary>
    public static string ReportFormatExtension(ReportFormat format) => format switch
    {
        ReportFormat.ReportHtml => "html",
        ReportFormat.ReportCsv => "csv",
        ReportFormat.ReportJson => "json",
        ReportFormat.ReportText => "txt",
        _ => "txt",
    };

    /// <summary>
    /// A plain-English summary of what a redaction selection will strip, for the
    /// export dialog so the user knows before generating. Returns "Nothing will be
    /// removed — the report includes all details." when no option is set.
    /// </summary>
    public static string RedactionSummary(RedactionOptions options)
    {
        var parts = new List<string>();
        if (options.RedactUserNames)
        {
            parts.Add("user names");
        }
        if (options.RedactComputerName)
        {
            parts.Add("computer name");
        }
        if (options.RedactPaths)
        {
            parts.Add("file paths");
        }
        if (options.RedactCommandLines)
        {
            parts.Add("command lines");
        }

        if (parts.Count == 0)
        {
            return "Nothing will be removed — the report includes all details.";
        }
        return "Will remove: " + string.Join(", ", parts) + ".";
    }
}
