using System.Globalization;
using Atlas.V0;

namespace Atlas.IpcClient;

/// <summary>
/// Pure mapping/formatting helpers for the M6 history surface (timeline, event
/// lane, safe-action risk text). Kept free of I/O and of any WinUI type so the
/// view-models stay thin and the logic is unit-testable without a live server
/// (task brief §1). All rendering decisions that the PRD pins — gaps as breaks
/// not zeros (§11.3), risk wording (§9.22) — live here.
/// </summary>
public static class HistoryFormatter
{
    private static readonly CultureInfo Inv = CultureInfo.InvariantCulture;

    /// <summary>Permille (0..1000) → percent (0..100).</summary>
    public static double Percent(double permille) => permille / 10.0;

    /// <summary>
    /// A single normalized timeline point: the bucket's start time and its
    /// min/max/avg already converted from permille to percent. The band is
    /// [<see cref="MinPercent"/>, <see cref="MaxPercent"/>]; the line is
    /// <see cref="AvgPercent"/>.
    /// </summary>
    public readonly record struct TimelinePoint(
        long StartMs, double MinPercent, double MaxPercent, double AvgPercent, uint Samples);

    /// <summary>
    /// Maps CPU-permille range buckets to percent-scaled timeline points, in
    /// time order. Buckets the server omitted (gaps) are simply absent — the
    /// renderer must break the polyline across them, never draw zero (§11.3).
    /// </summary>
    public static IReadOnlyList<TimelinePoint> ToCpuTimeline(IEnumerable<RangeBucket> buckets)
    {
        var points = new List<TimelinePoint>();
        foreach (var b in buckets)
        {
            points.Add(new TimelinePoint(
                b.StartMs,
                Percent(b.Min),
                Percent(b.Max),
                Percent(b.Avg),
                b.Samples));
        }
        points.Sort((a, c) => a.StartMs.CompareTo(c.StartMs));
        return points;
    }

    /// <summary>
    /// True when two adjacent buckets are farther apart than
    /// <paramref name="expectedStepMs"/> allows (with a 50% tolerance),
    /// i.e. there is a data gap between them that must render as a break.
    /// </summary>
    public static bool IsGap(long earlierStartMs, long laterStartMs, long expectedStepMs) =>
        expectedStepMs > 0 && (laterStartMs - earlierStartMs) > expectedStepMs + expectedStepMs / 2;

    /// <summary>Proc-event kind (0=start, 1=stop) → display verb.</summary>
    public static string EventKindLabel(uint kind) => kind switch
    {
        0 => "started",
        1 => "exited",
        _ => "event",
    };

    /// <summary>
    /// One-line description of a process start/stop event, e.g.
    /// <c>chrome.exe (pid 4242) started</c> or
    /// <c>notepad.exe (pid 900) exited (code 1)</c>.
    /// </summary>
    public static string EventLine(EventRow e)
    {
        var name = string.IsNullOrEmpty(e.ImageName) ? "(unknown)" : e.ImageName;
        var baseText = string.Format(
            Inv, "{0} (pid {1}) {2}", name, e.Pid, EventKindLabel(e.Kind));
        return e.HasExitStatus
            ? string.Format(Inv, "{0} (code {1})", baseText, e.ExitStatus)
            : baseText;
    }

    // ----------------------------------------------------------------------
    // Safe-action risk summary (PRD §9.22). Pure text so the dialog VM is
    // unit-testable with a fake risk (task brief §4).
    // ----------------------------------------------------------------------

    /// <summary>The affirmative-button caption for an action kind.</summary>
    public static string ActionVerb(ProcessActionKind action) => action switch
    {
        ProcessActionKind.CloseWindows => "Close",
        ProcessActionKind.Suspend => "Suspend",
        ProcessActionKind.Resume => "Resume",
        ProcessActionKind.Terminate => "End",
        _ => "Apply",
    };

    /// <summary>
    /// Whether the action is reversible: Suspend/Resume/Close leave the process
    /// (or its state) recoverable; Terminate does not. Surfaced in the dialog so
    /// the user understands the stakes before the one affirmative button.
    /// </summary>
    public static bool IsReversible(ProcessActionKind action) =>
        action != ProcessActionKind.Terminate;

    /// <summary>Human phrase for reversibility, e.g. for the dialog subtitle.</summary>
    public static string ReversibilityText(ProcessActionKind action) =>
        IsReversible(action)
            ? "This action is reversible."
            : "This action is not reversible — the process ends immediately.";

    /// <summary>
    /// A compact multi-line risk summary from an <see cref="ActionRisk"/>: the
    /// critical/system flags, visible-window and child counts, and the broker's
    /// human-readable notes. Empty string when there is nothing notable.
    /// </summary>
    public static string RiskSummary(ActionRisk? risk)
    {
        if (risk is null)
        {
            return string.Empty;
        }

        var lines = new List<string>();
        if (risk.IsCritical)
        {
            lines.Add("• Protected critical process — the system may become unstable.");
        }
        if (risk.IsSystem)
        {
            lines.Add("• Owned by SYSTEM (session 0).");
        }
        if (risk.VisibleWindows > 0)
        {
            lines.Add(string.Format(
                Inv,
                "• {0} visible window{1} — unsaved work may be lost.",
                risk.VisibleWindows,
                risk.VisibleWindows == 1 ? string.Empty : "s"));
        }
        if (risk.ChildCount > 0)
        {
            lines.Add(string.Format(
                Inv,
                "• {0} child process{1}.",
                risk.ChildCount,
                risk.ChildCount == 1 ? string.Empty : "es"));
        }
        foreach (var note in risk.Notes)
        {
            if (!string.IsNullOrWhiteSpace(note))
            {
                lines.Add("• " + note);
            }
        }
        return string.Join("\n", lines);
    }
}
