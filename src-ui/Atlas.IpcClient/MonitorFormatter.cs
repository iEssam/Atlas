using System.Globalization;
using Atlas.V0;

namespace Atlas.IpcClient;

/// <summary>
/// Pure mapping/formatting helpers for the R2 monitors surface (network
/// connections + listening ports, scheduled tasks, boot analysis, battery,
/// thermal). Free of I/O and of any WinUI type so the view-models stay thin and
/// the logic is unit-testable without a live server (task brief §1).
///
/// <para>
/// Tone matters here as everywhere in Atlas (PRD §9.6.7 honesty principle). A
/// connection state, a task's last result, an absent battery or an empty thermal
/// list are <b>information, not accusations</b>: the state color tokens keep even
/// "closing"/"time-wait" at a calm <em>transitional</em> rather than an alarming
/// red, a battery that isn't present reads as "desktop system" not a fault, and a
/// machine that exposes no thermal sensors is stated plainly.
/// </para>
/// </summary>
public static class MonitorFormatter
{
    private static readonly CultureInfo Inv = CultureInfo.InvariantCulture;

    // ----------------------------------------------------------------------
    // Network — protocol / state / endpoints (PRD §9.12).
    // ----------------------------------------------------------------------

    /// <summary>An L4 protocol's short label: "TCP" / "UDP" / "—" (unspecified).</summary>
    public static string L4ProtocolLabel(L4Protocol protocol) => protocol switch
    {
        L4Protocol.Tcp => "TCP",
        L4Protocol.Udp => "UDP",
        _ => "—",
    };

    /// <summary>
    /// A friendly TCP-state label. UDP has no state, so an unspecified value
    /// (typical for UDP rows) renders as an em-dash rather than "Unknown".
    /// </summary>
    public static string TcpStateLabel(TcpState state) => state switch
    {
        TcpState.TcpClosed => "Closed",
        TcpState.TcpListen => "Listening",
        TcpState.TcpSynSent => "SYN sent",
        TcpState.TcpSynRcvd => "SYN received",
        TcpState.TcpEstablished => "Established",
        TcpState.TcpFinWait1 => "FIN wait 1",
        TcpState.TcpFinWait2 => "FIN wait 2",
        TcpState.TcpCloseWait => "Close wait",
        TcpState.TcpClosing => "Closing",
        TcpState.TcpLastAck => "Last ACK",
        TcpState.TcpTimeWait => "Time wait",
        TcpState.TcpDeleteTcb => "Deleting",
        _ => "—",
    };

    /// <summary>
    /// A neutral color token for a TCP state, so XAML can pick a calm color without
    /// embedding policy: "active" (established — a positive, connected state),
    /// "listen" (listening — informational/accent), "transitional" (every handshake
    /// / teardown state), "idle" (closed), "none" (unspecified — e.g. UDP). No state
    /// maps to a danger/alarm token: a socket in TIME_WAIT is normal, not a warning
    /// (task brief §1).
    /// </summary>
    public static string TcpStateToken(TcpState state) => state switch
    {
        TcpState.TcpEstablished => "active",
        TcpState.TcpListen => "listen",
        TcpState.TcpSynSent
            or TcpState.TcpSynRcvd
            or TcpState.TcpFinWait1
            or TcpState.TcpFinWait2
            or TcpState.TcpCloseWait
            or TcpState.TcpClosing
            or TcpState.TcpLastAck
            or TcpState.TcpTimeWait
            or TcpState.TcpDeleteTcb => "transitional",
        TcpState.TcpClosed => "idle",
        _ => "none",
    };

    /// <summary>
    /// Formats an "address:port" endpoint, bracketing IPv6 literals so the port is
    /// unambiguous, e.g. <c>[fe80::1]:443</c> vs <c>10.0.0.5:443</c>. A blank
    /// address becomes "*" (any/unspecified bind). A zero port is elided so a
    /// listening UDP endpoint reads as just the address.
    /// </summary>
    public static string EndpointText(string? address, uint port, bool isIpv6)
    {
        var addr = string.IsNullOrWhiteSpace(address) ? "*" : address!.Trim();
        // Bracket only real IPv6 literals (contain a colon); "*" and IPv4 stay bare.
        if (isIpv6 && addr != "*" && addr.Contains(':') && !addr.StartsWith('['))
        {
            addr = "[" + addr + "]";
        }
        return port == 0 ? addr : string.Format(Inv, "{0}:{1}", addr, port);
    }

    /// <summary>
    /// A remote-domain display for a connection: the resolved domain, or an em-dash
    /// when the DNS cache had no name (blank is "not resolved", not an error).
    /// </summary>
    public static string DomainText(string? domain) =>
        string.IsNullOrWhiteSpace(domain) ? "—" : domain!.Trim();

    /// <summary>
    /// A process display for a connection/port row: the image name, falling back to
    /// "pid N" when the image name is blank, and appending the PID for
    /// disambiguation when both are present.
    /// </summary>
    public static string ProcessText(string? imageName, uint pid)
    {
        if (string.IsNullOrWhiteSpace(imageName))
        {
            return pid == 0 ? "—" : string.Format(Inv, "pid {0}", pid);
        }
        return pid == 0 ? imageName!.Trim() : string.Format(Inv, "{0} ({1})", imageName!.Trim(), pid);
    }

    // ----------------------------------------------------------------------
    // Scheduled tasks (PRD §9.9.2).
    // ----------------------------------------------------------------------

    /// <summary>Enabled/disabled pill caption for a task.</summary>
    public static string TaskEnabledLabel(bool enabled) => enabled ? "Enabled" : "Disabled";

    /// <summary>A triggers summary, or a calm placeholder when a task has none.</summary>
    public static string TriggersText(string? triggers) =>
        string.IsNullOrWhiteSpace(triggers) ? "No triggers" : triggers!.Trim();

    /// <summary>The task's primary action (exe + args), or an em-dash when blank.</summary>
    public static string ActionText(string? action) =>
        string.IsNullOrWhiteSpace(action) ? "—" : action!.Trim();

    /// <summary>The task author, or "Unknown" when the service couldn't determine one.</summary>
    public static string AuthorText(string? author) =>
        string.IsNullOrWhiteSpace(author) ? "Unknown" : author!.Trim();

    /// <summary>
    /// A friendly last-result string for a scheduled task. 0 is "Success"; the two
    /// common "has not run / is running" sentinels are named; anything else is shown
    /// as its 0x hex code so it is greppable against Task Scheduler docs. This is a
    /// fact, not a verdict — a non-zero result is stated calmly.
    /// </summary>
    public static string TaskLastResultText(int lastResult)
    {
        return lastResult switch
        {
            0 => "Success",
            // SCHED_S_TASK_HAS_NOT_RUN (0x00041303) and SCHED_S_TASK_RUNNING
            // (0x00041301) — common, benign states rather than failures.
            0x00041303 => "Never run",
            0x00041301 => "Running",
            0x00041306 => "Terminated",
            _ => "0x" + ((uint)lastResult).ToString("X8", Inv),
        };
    }

    /// <summary>
    /// A neutral color token for a last-result, so XAML picks a calm color: "ok"
    /// (success), "idle" (never run / running / terminated — informational), and
    /// "attention" for any other non-zero code. "attention" is a gentle caution,
    /// never an alarm — a task can legitimately return a non-zero code (task §1).
    /// </summary>
    public static string TaskLastResultToken(int lastResult) => lastResult switch
    {
        0 => "ok",
        0x00041303 or 0x00041301 or 0x00041306 => "idle",
        _ => "attention",
    };

    /// <summary>
    /// A last-run display for a task. A non-positive timestamp (never run) is stated
    /// as "Never" rather than an epoch date. Otherwise a relative "…ago" phrase.
    /// </summary>
    public static string LastRunText(long lastRunMs, long nowMs)
    {
        if (lastRunMs <= 0)
        {
            return "Never";
        }
        return RelativePast(lastRunMs, nowMs);
    }

    /// <summary>
    /// A next-run display for a task. A non-positive timestamp (no scheduled next
    /// run — e.g. an on-demand or disabled task) is stated as "Not scheduled".
    /// Otherwise a relative "in …" phrase (or "Due now" if already past).
    /// </summary>
    public static string NextRunText(long nextRunMs, long nowMs)
    {
        if (nextRunMs <= 0)
        {
            return "Not scheduled";
        }
        return RelativeFuture(nextRunMs, nowMs);
    }

    /// <summary>
    /// A compact relative phrase for a past instant: "just now", "5m ago", "2h ago",
    /// "3d ago", "2w ago". Pure (takes <paramref name="nowMs"/>) for deterministic
    /// testing. Future/non-positive deltas collapse to "just now".
    /// </summary>
    public static string RelativePast(long tsMs, long nowMs)
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
        if (days < 7)
        {
            return string.Format(Inv, "{0}d ago", days);
        }
        long weeks = days / 7;
        return string.Format(Inv, "{0}w ago", weeks);
    }

    /// <summary>
    /// A compact relative phrase for a future instant: "Due now", "in 5m", "in 2h",
    /// "in 3d", "in 2w". Pure for deterministic testing. Past/now deltas collapse to
    /// "Due now".
    /// </summary>
    public static string RelativeFuture(long tsMs, long nowMs)
    {
        long deltaMs = tsMs - nowMs;
        if (deltaMs <= 0)
        {
            return "Due now";
        }
        long seconds = deltaMs / 1000;
        if (seconds < 60)
        {
            return "in <1m";
        }
        long minutes = seconds / 60;
        if (minutes < 60)
        {
            return string.Format(Inv, "in {0}m", minutes);
        }
        long hours = minutes / 60;
        if (hours < 24)
        {
            return string.Format(Inv, "in {0}h", hours);
        }
        long days = hours / 24;
        if (days < 7)
        {
            return string.Format(Inv, "in {0}d", days);
        }
        long weeks = days / 7;
        return string.Format(Inv, "in {0}w", weeks);
    }

    /// <summary>Run-level pill caption: whether the task runs with highest privileges.</summary>
    public static string RunLevelText(bool runAsHighest) =>
        runAsHighest ? "Highest privileges" : "Limited privileges";

    // ----------------------------------------------------------------------
    // Boot analysis (PRD §9.6.6).
    // ----------------------------------------------------------------------

    /// <summary>
    /// A boot duration in whole seconds/minutes from milliseconds, e.g. 72000 →
    /// "1m 12s", 8000 → "8s", 0 → "—". Compact and human, matching the process
    /// inspector's duration style.
    /// </summary>
    public static string BootDurationText(uint ms)
    {
        if (ms == 0)
        {
            return "—";
        }
        long totalSeconds = ms / 1000;
        if (totalSeconds < 60)
        {
            // Show at least "1s" for any non-zero sub-second duration.
            return string.Format(Inv, "{0}s", Math.Max(1, totalSeconds));
        }
        long minutes = totalSeconds / 60;
        long seconds = totalSeconds % 60;
        return string.Format(Inv, "{0}m {1}s", minutes, seconds);
    }

    /// <summary>
    /// A calm degraded-flag caption for a boot: "Slower than usual" when flagged,
    /// "Normal" otherwise. "Slower than usual" describes the measurement without
    /// alarm — a single slow boot is not a problem to fix (task §4 tone).
    /// </summary>
    public static string BootDegradedText(bool degraded) =>
        degraded ? "Slower than usual" : "Normal";

    /// <summary>Color token for a boot's degraded flag: "attention" vs "ok".</summary>
    public static string BootDegradedToken(bool degraded) => degraded ? "attention" : "ok";

    /// <summary>An absolute date/time for a boot instant (local), e.g. "Jul 13, 08:41".</summary>
    public static string BootTimeText(long bootMs)
    {
        if (bootMs <= 0)
        {
            return "—";
        }
        var local = DateTimeOffset.FromUnixTimeMilliseconds(bootMs).LocalDateTime;
        return local.ToString("MMM d, HH:mm", Inv);
    }

    // ----------------------------------------------------------------------
    // Battery (PRD §9.6.7).
    // ----------------------------------------------------------------------

    /// <summary>A battery charge percent, e.g. 87 → "87%".</summary>
    public static string BatteryPercentText(uint percent) =>
        string.Format(Inv, "{0}%", percent);

    /// <summary>
    /// A one-line power-state summary combining charge direction, rate (W), and
    /// estimated time left, e.g. "Discharging 12.4 W, ~2h 40m left",
    /// "Charging 30.1 W", "On AC power" (full / not discharging), "Fully charged".
    /// The <paramref name="rateMw"/> is milliwatts (negative = discharging); a rate
    /// of 0 hides the wattage clause. <paramref name="estRuntimeS"/> ≤ 0 hides the
    /// time-left clause.
    /// </summary>
    public static string BatteryStateSummary(bool charging, bool onAc, int rateMw, long estRuntimeS)
    {
        double watts = Math.Abs(rateMw) / 1000.0;
        var rateClause = rateMw == 0
            ? string.Empty
            : string.Format(Inv, " {0:0.0} W", watts);

        if (charging)
        {
            return "Charging" + rateClause;
        }

        // Not charging. If discharging (negative rate) we're on battery.
        if (rateMw < 0)
        {
            var left = estRuntimeS > 0 ? ", ~" + RuntimeText(estRuntimeS) + " left" : string.Empty;
            return "Discharging" + rateClause + left;
        }

        // Not charging and not discharging: plugged in and topped up, or idle on AC.
        return onAc ? "On AC power" : "Not charging";
    }

    /// <summary>
    /// An estimated-runtime phrase from seconds: "2h 40m", "45m", "&lt;1m". Used
    /// inside <see cref="BatteryStateSummary"/> and standalone. Non-positive → "—".
    /// </summary>
    public static string RuntimeText(long seconds)
    {
        if (seconds <= 0)
        {
            return "—";
        }
        long minutes = seconds / 60;
        if (minutes < 1)
        {
            return "<1m";
        }
        if (minutes < 60)
        {
            return string.Format(Inv, "{0}m", minutes);
        }
        long hours = minutes / 60;
        long remMin = minutes % 60;
        return string.Format(Inv, "{0}h {1}m", hours, remMin);
    }

    /// <summary>
    /// A battery health line from full-charge vs design capacity, e.g.
    /// "Health 92% (of design)". When <paramref name="healthPercent"/> is 0
    /// (not derivable — some batteries don't report design capacity) returns
    /// "Health not reported" rather than a misleading "0%".
    /// </summary>
    public static string BatteryHealthText(uint healthPercent)
    {
        if (healthPercent == 0)
        {
            return "Health not reported";
        }
        return string.Format(Inv, "Health {0}% (of design)", healthPercent);
    }

    /// <summary>
    /// A cycle-count line, e.g. "412 cycles". 0 (not reported) → "Cycle count not
    /// reported" — many batteries/firmwares don't expose it, which isn't an error.
    /// </summary>
    public static string CycleCountText(uint cycleCount)
    {
        if (cycleCount == 0)
        {
            return "Cycle count not reported";
        }
        return string.Format(Inv, "{0} cycle{1}", cycleCount, cycleCount == 1 ? "" : "s");
    }

    // ----------------------------------------------------------------------
    // Thermal (PRD §9.6.7).
    // ----------------------------------------------------------------------

    /// <summary>A temperature in °C with one decimal, e.g. 42.5 → "42.5 °C".</summary>
    public static string TemperatureText(double celsius) =>
        string.Format(Inv, "{0:0.0} °C", celsius);

    /// <summary>A thermal sensor's source, or an em-dash when the source is blank.</summary>
    public static string ThermalSourceText(string? source) =>
        string.IsNullOrWhiteSpace(source) ? "—" : source!.Trim();

    // ----------------------------------------------------------------------
    // Shared "available = false" honesty helper (task brief §4).
    // ----------------------------------------------------------------------

    /// <summary>
    /// A friendly line for an <c>available = false</c> reply, preferring the
    /// service's own <c>unavailable_reason</c> and falling back to
    /// <paramref name="fallback"/>. Kept identical in spirit to R2Formatter's
    /// version so both surfaces read the same, calm way.
    /// </summary>
    public static string UnavailableReason(string? reason, string fallback) =>
        string.IsNullOrWhiteSpace(reason) ? fallback : reason!.Trim();
}
