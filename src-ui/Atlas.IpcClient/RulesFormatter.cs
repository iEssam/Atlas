using System.Globalization;
using Atlas.V0;

namespace Atlas.IpcClient;

/// <summary>
/// Pure mapping/formatting helpers for the R2 rules-engine surface (rules,
/// simulation targets, active interventions, profiles — PRD §9.7). Free of I/O
/// and of any WinUI type so the view-models stay thin and the logic is
/// unit-testable without a live server (task brief §1).
///
/// <para>
/// Tone matters here as much as anywhere in Atlas. A rule is a persistent,
/// reversible policy; the summaries state plainly <em>what</em> a rule does and
/// <em>when</em>, never dramatising it. A blocked simulation target is framed as
/// protection ("Atlas won't touch this"), not as a failure. "Unchanged" is a
/// first-class value — a rule that only sets priority leaves affinity and eco
/// alone, and the summary says so rather than inventing a change.
/// </para>
/// </summary>
public static class RulesFormatter
{
    private static readonly CultureInfo Inv = CultureInfo.InvariantCulture;

    /// <summary>The calm placeholder for an action field a rule leaves untouched.</summary>
    public const string Unchanged = "Unchanged";

    // ----------------------------------------------------------------------
    // Enum labels.
    // ----------------------------------------------------------------------

    /// <summary>
    /// A friendly priority-class label. <c>Unspecified</c> means "leave priority
    /// alone" and renders as <see cref="Unchanged"/>. REALTIME is deliberately not
    /// in the contract, so there is no case for it.
    /// </summary>
    public static string PriorityClassLabel(PriorityClass priority) => priority switch
    {
        PriorityClass.PriorityIdle => "Idle",
        PriorityClass.PriorityBelowNormal => "Below Normal",
        PriorityClass.PriorityNormal => "Normal",
        PriorityClass.PriorityAboveNormal => "Above Normal",
        PriorityClass.PriorityHigh => "High",
        _ => Unchanged,
    };

    /// <summary>
    /// A friendly core-affinity-mode label. <c>Unspecified</c> renders as
    /// <see cref="Unchanged"/>. The P/E labels use the compact "P-cores" /
    /// "E-cores" wording the rest of the UI uses for performance/efficiency cores.
    /// </summary>
    public static string CoreAffinityModeLabel(CoreAffinityMode mode) => mode switch
    {
        CoreAffinityMode.AllCores => "All cores",
        CoreAffinityMode.PreferPCores => "P-cores",
        CoreAffinityMode.PreferECores => "E-cores",
        CoreAffinityMode.CustomMask => "Custom cores",
        _ => Unchanged,
    };

    /// <summary>
    /// Friendly trigger text for a rule ("while running", "on AC power", "on
    /// battery", "in fullscreen apps"). <c>Unspecified</c> falls back to
    /// "while running" — the safe, always-on default a saved rule without an
    /// explicit trigger behaves as.
    /// </summary>
    public static string RuleTriggerText(RuleTrigger trigger) => trigger switch
    {
        RuleTrigger.WhileRunning => "while running",
        RuleTrigger.OnAcPower => "on AC power",
        RuleTrigger.OnDcPower => "on battery",
        RuleTrigger.OnFullscreen => "in fullscreen apps",
        RuleTrigger.OnGpuLoad => "after sustained GPU load",
        RuleTrigger.OnGpuThermalThrottle => "during GPU thermal throttling",
        _ => "while running",
    };

    /// <summary>
    /// A friendly power-mode label for a profile. The empty string means the
    /// profile does not touch the system power mode and renders as
    /// "No power-mode change".
    /// </summary>
    public static string PowerModeLabel(string? powerMode)
    {
        if (string.IsNullOrWhiteSpace(powerMode))
        {
            return "No power-mode change";
        }
        return powerMode switch
        {
            "PowerSaver" => "Power saver",
            "Balanced" => "Balanced",
            "HighPerformance" => "High performance",
            _ => powerMode!,
        };
    }

    // ----------------------------------------------------------------------
    // Affinity masks.
    // ----------------------------------------------------------------------

    /// <summary>
    /// A compact human rendering of a processor affinity bitmask, collapsing runs
    /// of consecutive cores into ranges: 0b1_0000_1111 → "cores 0-3,8". A zero mask
    /// (no cores selected) renders as "no cores" rather than an empty string so a
    /// custom mask never reads as a blank field.
    /// </summary>
    public static string AffinityMaskText(ulong mask)
    {
        if (mask == 0)
        {
            return "no cores";
        }

        var parts = new List<string>();
        int runStart = -1;
        int prev = -1;
        for (int bit = 0; bit < 64; bit++)
        {
            bool set = (mask & (1UL << bit)) != 0;
            if (set)
            {
                if (runStart < 0)
                {
                    runStart = bit;
                }
                prev = bit;
            }
            else if (runStart >= 0)
            {
                parts.Add(RangeText(runStart, prev));
                runStart = -1;
            }
        }
        if (runStart >= 0)
        {
            parts.Add(RangeText(runStart, prev));
        }

        return "cores " + string.Join(",", parts);
    }

    private static string RangeText(int start, int end) =>
        start == end
            ? start.ToString(Inv)
            : string.Format(Inv, "{0}-{1}", start, end);

    /// <summary>
    /// Parses a compact core list ("0-3,8") into a processor affinity bitmask —
    /// the inverse of <see cref="AffinityMaskText"/> for the custom-mask entry in
    /// the rule editor. Returns false (and a zero mask) on any malformed token, an
    /// out-of-range core (valid range 0..63), or a reversed range. An empty/blank
    /// string is rejected so the editor can require at least one core.
    /// </summary>
    public static bool TryParseCoreList(string? text, out ulong mask)
    {
        mask = 0;
        if (string.IsNullOrWhiteSpace(text))
        {
            return false;
        }

        foreach (var part in text.Split(',', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries))
        {
            int dash = part.IndexOf('-');
            if (dash < 0)
            {
                if (!TryParseCore(part, out int core))
                {
                    return false;
                }
                mask |= 1UL << core;
            }
            else
            {
                var loText = part.Substring(0, dash).Trim();
                var hiText = part.Substring(dash + 1).Trim();
                if (!TryParseCore(loText, out int lo) || !TryParseCore(hiText, out int hi) || hi < lo)
                {
                    return false;
                }
                for (int c = lo; c <= hi; c++)
                {
                    mask |= 1UL << c;
                }
            }
        }
        return true;
    }

    private static bool TryParseCore(string text, out int core)
    {
        if (int.TryParse(text, NumberStyles.Integer, Inv, out core) && core >= 0 && core <= 63)
        {
            return true;
        }
        core = 0;
        return false;
    }

    // ----------------------------------------------------------------------
    // Action / rule summaries.
    // ----------------------------------------------------------------------

    /// <summary>
    /// A one-line summary of a rule's action ("Below Normal, E-cores, Eco"). Only
    /// the parts a rule actually changes are listed; a rule that changes nothing
    /// (all-unspecified priority/affinity, eco off) reads as "No change" rather
    /// than an empty string. A CUSTOM affinity renders its mask compactly.
    /// </summary>
    public static string RuleActionSummary(RuleAction? action)
    {
        if (action is null)
        {
            return "No change";
        }

        var parts = new List<string>();

        if (action.Priority != PriorityClass.Unspecified)
        {
            parts.Add(PriorityClassLabel(action.Priority));
        }

        if (action.AffinityMode != CoreAffinityMode.CoreAffinityUnspecified)
        {
            parts.Add(action.AffinityMode == CoreAffinityMode.CustomMask
                ? AffinityMaskText(action.AffinityMask)
                : CoreAffinityModeLabel(action.AffinityMode));
        }

        if (action.EcoQos)
        {
            parts.Add("Eco");
        }

        return parts.Count == 0 ? "No change" : string.Join(", ", parts);
    }

    /// <summary>
    /// A one-line human summary of a whole rule:
    /// "chrome.exe → Below Normal, E-cores, Eco — while running". A rule with no
    /// image match reads as "(no match) → …" so an incomplete rule is obvious
    /// rather than silently matching nothing.
    /// </summary>
    public static string RuleSummary(Rule? rule)
    {
        if (rule is null)
        {
            return string.Empty;
        }

        var image = string.IsNullOrWhiteSpace(rule.MatchImage) ? "(no match)" : rule.MatchImage;
        return string.Format(
            Inv,
            "{0} → {1} — {2}",
            image,
            RuleActionSummary(rule.Action),
            RuleTriggerText(rule.Trigger));
    }

    /// <summary>The enabled/disabled pill caption for a rule.</summary>
    public static string EnabledLabel(bool enabled) => enabled ? "Enabled" : "Disabled";

    /// <summary>The active/inactive pill caption for a profile.</summary>
    public static string ActiveLabel(bool active) => active ? "Active" : "Inactive";

    // ----------------------------------------------------------------------
    // Simulation targets (the preview centerpiece — PRD §9.7.5).
    // ----------------------------------------------------------------------

    /// <summary>
    /// A "current → new" transition string for a simulated field, e.g.
    /// "Normal → Below Normal". When the two sides are equal (or the new side is
    /// blank, meaning "unchanged"), it collapses to just the current value so the
    /// preview doesn't shout a non-change.
    /// </summary>
    public static string TransitionText(string? current, string? next)
    {
        var from = string.IsNullOrWhiteSpace(current) ? "—" : current!.Trim();
        var to = string.IsNullOrWhiteSpace(next) ? string.Empty : next!.Trim();
        if (to.Length == 0 || string.Equals(from, to, StringComparison.OrdinalIgnoreCase))
        {
            return from;
        }
        return string.Format(Inv, "{0} → {1}", from, to);
    }

    /// <summary>
    /// The eco-mode change caption for a simulated target: "Eco on" when this rule
    /// would enable EcoQoS on the target, or an em-dash when it makes no eco
    /// change. Kept short for a dense preview row.
    /// </summary>
    public static string EcoChangeText(bool ecoChange) => ecoChange ? "Eco on" : "—";

    /// <summary>
    /// The reason a simulated target is protected from a rule, framed as Atlas
    /// protecting the system rather than the rule failing. Prefers the service's
    /// own <c>blocked_reason</c> and falls back to the standard line.
    /// </summary>
    public static string BlockedReasonText(string? blockedReason) =>
        string.IsNullOrWhiteSpace(blockedReason)
            ? "System-critical — Atlas won't touch this."
            : blockedReason!;

    /// <summary>
    /// A one-line summary of a simulation result for a status line, e.g.
    /// "3 processes affected, 1 protected" or "No running processes match".
    /// <paramref name="total"/> is the target count, <paramref name="blocked"/> the
    /// protected subset.
    /// </summary>
    public static string SimulationSummary(int total, int blocked)
    {
        if (total == 0)
        {
            return "No running processes match this rule right now.";
        }

        int affected = total - blocked;
        var affectedText = string.Format(
            Inv, "{0} process{1} would be affected", affected, affected == 1 ? string.Empty : "es");

        if (blocked == 0)
        {
            return affectedText + ".";
        }
        return string.Format(
            Inv,
            "{0}, {1} protected target{2} left untouched.",
            affectedText,
            blocked,
            blocked == 1 ? string.Empty : "s");
    }

    // ----------------------------------------------------------------------
    // Active interventions (the transparency surface — PRD §9.7.3).
    // ----------------------------------------------------------------------

    /// <summary>
    /// The "applied" summary for an active intervention, preferring the service's
    /// own human text and falling back to a neutral placeholder when it is blank.
    /// </summary>
    public static string InterventionApplied(string? applied) =>
        string.IsNullOrWhiteSpace(applied) ? "Policy applied" : applied!.Trim();

    /// <summary>
    /// A compact "since" phrase for an intervention that started at
    /// <paramref name="sinceMs"/> (Unix ms), measured against <paramref name="nowMs"/>.
    /// "just now" under a minute, then "3m", "2h", "4d". A non-positive or future
    /// timestamp reads as "just now" rather than a negative duration.
    /// </summary>
    public static string RelativeSince(long sinceMs, long nowMs)
    {
        long deltaMs = nowMs - sinceMs;
        if (sinceMs <= 0 || deltaMs < 60_000)
        {
            return "just now";
        }

        long minutes = deltaMs / 60_000;
        if (minutes < 60)
        {
            return string.Format(Inv, "{0}m", minutes);
        }
        long hours = minutes / 60;
        if (hours < 24)
        {
            return string.Format(Inv, "{0}h", hours);
        }
        long days = hours / 24;
        return string.Format(Inv, "{0}d", days);
    }

    /// <summary>
    /// A full one-line intervention line for a transparency list:
    /// "chrome.exe (pid 4242) — Below Normal, E-cores · via Gaming · 3m".
    /// </summary>
    public static string InterventionLine(Intervention? intervention, long nowMs)
    {
        if (intervention is null)
        {
            return string.Empty;
        }

        var image = string.IsNullOrWhiteSpace(intervention.ImageName)
            ? "(unknown)"
            : intervention.ImageName;
        var rule = string.IsNullOrWhiteSpace(intervention.RuleName)
            ? "a rule"
            : intervention.RuleName;

        return string.Format(
            Inv,
            "{0} (pid {1}) — {2} · via {3} · {4}",
            image,
            intervention.Pid,
            InterventionApplied(intervention.Applied),
            rule,
            RelativeSince(intervention.SinceMs, nowMs));
    }
}
