using System.Globalization;
using Atlas.V0;

namespace Atlas.IpcClient;

/// <summary>
/// Pure mapping/formatting + clamping helpers for the R3 dynamic responsiveness
/// protection surface (PRD §9.7.3). Free of I/O and of any WinUI type so the
/// view-model stays thin and the logic is unit-testable without a live server
/// (task brief §1).
///
/// <para>
/// The safety framing is the whole point of this feature, so the copy here is
/// calm and honest: protection is <em>temporary</em>, <em>auto-restored</em>, and
/// <em>never</em> touches the foreground or system-critical apps. The config
/// summary states plainly what the watchdog would do and never dramatises it. The
/// bounds keep a user from configuring something surprising (an absurdly low
/// threshold, a multi-hour dampening) — protection stays conservative by design,
/// and off by default.
/// </para>
/// </summary>
public static class DynamicProtectionFormatter
{
    private static readonly CultureInfo Inv = CultureInfo.InvariantCulture;

    // ----------------------------------------------------------------------
    // Sane bounds + defaults (PRD §9.7.3: conservative, off by default). The
    // CPU threshold is a share of TOTAL system CPU a background app must exceed
    // to even be a candidate; sustain is how long it must stay there first; max
    // is the hard auto-restore cap. The UI edits percent + seconds; the wire
    // carries permille + seconds.
    // ----------------------------------------------------------------------

    /// <summary>A background app must exceed at least this CPU share to be a candidate.</summary>
    public const double MinThresholdPercent = 10.0;

    /// <summary>The threshold is capped below 100% — nothing can exceed the whole machine.</summary>
    public const double MaxThresholdPercent = 99.0;

    /// <summary>The default candidate threshold (a clearly-monopolising share).</summary>
    public const double DefaultThresholdPercent = 80.0;

    /// <summary>The shortest sustain window (avoids reacting to a brief spike).</summary>
    public const uint MinSustainSeconds = 5;

    /// <summary>The longest sustain window the UI offers.</summary>
    public const uint MaxSustainSeconds = 600;

    /// <summary>The default sustain window before any intervention.</summary>
    public const uint DefaultSustainSeconds = 30;

    /// <summary>The shortest auto-restore cap.</summary>
    public const uint MinMaxInterventionSeconds = 10;

    /// <summary>The longest auto-restore cap the UI offers (one hour).</summary>
    public const uint MaxMaxInterventionSeconds = 3600;

    /// <summary>The default auto-restore cap (five minutes).</summary>
    public const uint DefaultMaxInterventionSeconds = 300;

    // ----------------------------------------------------------------------
    // Permille <-> percent.
    // ----------------------------------------------------------------------

    /// <summary>A CPU share in permille (per-thousand) as a percent value: 805 → 80.5.</summary>
    public static double PermilleToPercent(uint permille) => permille / 10.0;

    /// <summary>A percent value as permille, rounded to the nearest tenth of a percent: 80.5 → 805.</summary>
    public static uint PercentToPermille(double percent)
    {
        if (percent < 0)
        {
            return 0;
        }
        return (uint)System.Math.Round(percent * 10.0, System.MidpointRounding.AwayFromZero);
    }

    // ----------------------------------------------------------------------
    // Clamping (the editor binds to raw numbers; these keep saves in-bounds).
    // ----------------------------------------------------------------------

    /// <summary>Clamps a percent to the offered range.</summary>
    public static double ClampThresholdPercent(double percent) =>
        percent < MinThresholdPercent ? MinThresholdPercent
        : percent > MaxThresholdPercent ? MaxThresholdPercent
        : percent;

    /// <summary>Clamps a sustain duration (seconds) to the offered range.</summary>
    public static uint ClampSustainSeconds(uint seconds) =>
        seconds < MinSustainSeconds ? MinSustainSeconds
        : seconds > MaxSustainSeconds ? MaxSustainSeconds
        : seconds;

    /// <summary>Clamps an auto-restore cap (seconds) to the offered range.</summary>
    public static uint ClampMaxInterventionSeconds(uint seconds) =>
        seconds < MinMaxInterventionSeconds ? MinMaxInterventionSeconds
        : seconds > MaxMaxInterventionSeconds ? MaxMaxInterventionSeconds
        : seconds;

    // ----------------------------------------------------------------------
    // Display text.
    // ----------------------------------------------------------------------

    /// <summary>
    /// A CPU threshold (permille) as a compact percent label: 800 → "80%",
    /// 805 → "80.5%". Whole percents drop the ".0"; a fractional share keeps one
    /// decimal so the value never looks rounded away.
    /// </summary>
    public static string ThresholdPercentText(uint permille)
    {
        double percent = PermilleToPercent(permille);
        return PercentText(percent);
    }

    /// <summary>A percent value as a compact label ("80%", "80.5%").</summary>
    public static string PercentText(double percent)
    {
        // One decimal at most; trim a trailing ".0" so 80.0 → "80".
        string body = percent.ToString("0.#", Inv);
        return body + "%";
    }

    /// <summary>
    /// A compact duration for a seconds value: "45s", "5m", "1m 30s", "2h". Zero
    /// reads as "0s". Kept short for dense helper lines; whole minutes/hours drop
    /// the remainder.
    /// </summary>
    public static string DurationText(uint seconds)
    {
        if (seconds < 60)
        {
            return string.Format(Inv, "{0}s", seconds);
        }

        if (seconds < 3600)
        {
            uint minutes = seconds / 60;
            uint rem = seconds % 60;
            return rem == 0
                ? string.Format(Inv, "{0}m", minutes)
                : string.Format(Inv, "{0}m {1}s", minutes, rem);
        }

        uint hours = seconds / 3600;
        uint minsRem = (seconds % 3600) / 60;
        return minsRem == 0
            ? string.Format(Inv, "{0}h", hours)
            : string.Format(Inv, "{0}h {1}m", hours, minsRem);
    }

    /// <summary>
    /// A one-line, plain-language summary of a config — what the watchdog would
    /// do, framed as the calm, reversible thing it is. When disabled it reads
    /// "Off — Atlas isn't easing back any app." A null config reads as off.
    /// </summary>
    public static string ConfigSummary(DynamicProtectionConfig? config)
    {
        if (config is null || !config.Enabled)
        {
            return "Off — Atlas isn't easing back any app.";
        }

        return string.Format(
            Inv,
            "On — eases back a background app that stays above {0} CPU for {1}, and restores it within {2}.",
            ThresholdPercentText(config.CpuThresholdPermille),
            DurationText(config.SustainSeconds),
            DurationText(config.MaxInterventionSeconds));
    }
}
