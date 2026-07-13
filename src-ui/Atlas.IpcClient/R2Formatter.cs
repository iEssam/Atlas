using System.Globalization;
using Atlas.V0;

namespace Atlas.IpcClient;

/// <summary>
/// Pure mapping/formatting helpers for the R2 process-inspector surface (process
/// detail, handles, modules, threads, resource owners). Free of I/O and of any
/// WinUI type so the view-models stay thin and the logic is unit-testable without
/// a live server (task brief §1).
///
/// <para>
/// Tone matters here as much as anywhere in Atlas. A blank field or an
/// <c>Unsigned</c> signature is <b>information, not an accusation</b> (PRD
/// §9.6.7 honesty principle): the trust tokens deliberately keep "unsigned" at a
/// calm <em>caution</em>, never the alarming red reserved for genuine danger, and
/// the coverage notes explain a limited view as an elevation gap rather than
/// implying the process is hiding something.
/// </para>
/// </summary>
public static class R2Formatter
{
    private static readonly CultureInfo Inv = CultureInfo.InvariantCulture;

    /// <summary>
    /// The standard calm note shown when a reply reports partial coverage
    /// (<c>limited</c>). It frames the gap as an elevation limitation, never as
    /// something suspicious about the process (task brief §2).
    /// </summary>
    public const string LimitedCoverageMessage =
        "Some details need an elevated Atlas — run as administrator for full coverage.";

    // ----------------------------------------------------------------------
    // Identity / security (ProcessDetail).
    // ----------------------------------------------------------------------

    /// <summary>
    /// A friendly integrity-level label. The proto carries the raw Windows label
    /// ("System" / "High" / "Medium" / "Low" / "AppContainer" / "" if unknown);
    /// this normalizes casing and maps the empty/unknown case to "Unknown".
    /// </summary>
    public static string IntegrityLabel(string? integrityLevel)
    {
        if (string.IsNullOrWhiteSpace(integrityLevel))
        {
            return "Unknown";
        }
        return integrityLevel switch
        {
            "System" => "System",
            "High" => "High",
            "Medium" => "Medium",
            "Low" => "Low",
            "AppContainer" => "AppContainer",
            _ => integrityLevel,
        };
    }

    /// <summary>Elevation pill caption. Elevated is a fact, not a warning.</summary>
    public static string ElevationLabel(bool elevated) => elevated ? "Elevated" : "Not elevated";

    /// <summary>
    /// A friendly architecture label. The proto carries "x64" / "x86" / "Arm64"
    /// / "" — the empty case becomes "Unknown".
    /// </summary>
    public static string ArchitectureLabel(string? architecture) =>
        string.IsNullOrWhiteSpace(architecture) ? "Unknown" : architecture;

    /// <summary>
    /// A friendly signature-status label. The proto carries "Signed (Microsoft)"
    /// / "Signed" / "Unsigned" / "Unknown"; the empty case becomes "Unknown".
    /// </summary>
    public static string SignatureStatusLabel(string? signatureStatus) =>
        string.IsNullOrWhiteSpace(signatureStatus) ? "Unknown" : signatureStatus;

    /// <summary>
    /// A neutral trust token for a signature status, so XAML can pick a color
    /// without embedding policy: "trusted" (Microsoft-signed), "signed"
    /// (third-party signed), "caution" (unsigned), "unknown" (couldn't tell).
    /// <b>Unsigned maps to "caution", never a danger token</b> — an unsigned
    /// binary is common and legitimate; the UI states the fact calmly and lets
    /// the user judge (task brief §1).
    /// </summary>
    public static string SignatureTrustToken(string? signatureStatus)
    {
        if (string.IsNullOrWhiteSpace(signatureStatus))
        {
            return "unknown";
        }
        if (signatureStatus.StartsWith("Signed (Microsoft", StringComparison.OrdinalIgnoreCase))
        {
            return "trusted";
        }
        if (signatureStatus.StartsWith("Signed", StringComparison.OrdinalIgnoreCase))
        {
            return "signed";
        }
        if (signatureStatus.Equals("Unsigned", StringComparison.OrdinalIgnoreCase))
        {
            return "caution";
        }
        return "unknown";
    }

    /// <summary>
    /// A publisher display: the raw publisher, or "Unknown publisher" when the
    /// service couldn't determine one (blank is not an accusation — task §2).
    /// </summary>
    public static string PublisherText(string? publisher) =>
        string.IsNullOrWhiteSpace(publisher) ? "Unknown publisher" : publisher!;

    /// <summary>
    /// Packaged (MSIX/AppX) vs. classic desktop indicator from the package
    /// identity: empty identity ⇒ "Desktop app", else the packaged identity.
    /// </summary>
    public static string PackageText(string? packageIdentity) =>
        string.IsNullOrWhiteSpace(packageIdentity) ? "Desktop app" : packageIdentity!;

    /// <summary>A user display: the account name, falling back to the SID, then "Unknown".</summary>
    public static string UserText(string? userName, string? userSid)
    {
        if (!string.IsNullOrWhiteSpace(userName))
        {
            return userName!;
        }
        return string.IsNullOrWhiteSpace(userSid) ? "Unknown" : userSid!;
    }

    /// <summary>A value or an em-dash placeholder when blank (for a detail row).</summary>
    public static string OrDash(string? value) =>
        string.IsNullOrWhiteSpace(value) ? "—" : value!;

    // ----------------------------------------------------------------------
    // Addresses / access masks / sizes (Handles, Modules, Threads).
    // ----------------------------------------------------------------------

    /// <summary>
    /// A fixed-width 64-bit hex address, e.g. 0x00007FFAB1230000. A zero address
    /// (unavailable start address) renders as an em-dash rather than a misleading
    /// "0x0" so the table reads as "not known" instead of "address zero".
    /// </summary>
    public static string AddressText(ulong address) =>
        address == 0 ? "—" : "0x" + address.ToString("X16", Inv);

    /// <summary>
    /// A handle value in hex, e.g. 0x1A4. Handle values are small, so this is not
    /// zero-padded to 16 like a virtual address.
    /// </summary>
    public static string HandleText(ulong handle) => "0x" + handle.ToString("X", Inv);

    /// <summary>
    /// A granted-access mask as compact hex, e.g. "0x1F0FFF". Zero renders as
    /// "0x0" (a real, if unusual, value — distinct from an unknown address).
    /// </summary>
    public static string GrantedAccessText(uint mask) => "0x" + mask.ToString("X", Inv);

    /// <summary>
    /// A short human summary of the standard-rights portion of an access mask,
    /// e.g. "Read, Synchronize" or "Full access". Returns an empty string when no
    /// standard right is set (the hex from <see cref="GrantedAccessText"/> stands
    /// alone). This decodes only the type-independent standard rights so it is
    /// correct for any object type.
    /// </summary>
    public static string AccessRightsSummary(uint mask)
    {
        const uint Delete = 0x00010000;
        const uint ReadControl = 0x00020000;
        const uint WriteDac = 0x00040000;
        const uint WriteOwner = 0x00080000;
        const uint Synchronize = 0x00100000;
        const uint StandardRightsAll = 0x001F0000;
        const uint GenericRead = 0x80000000;
        const uint GenericWrite = 0x40000000;
        const uint GenericExecute = 0x20000000;
        const uint GenericAll = 0x10000000;

        var parts = new List<string>();

        if ((mask & GenericAll) != 0)
        {
            parts.Add("Generic all");
        }
        if ((mask & GenericRead) != 0)
        {
            parts.Add("Generic read");
        }
        if ((mask & GenericWrite) != 0)
        {
            parts.Add("Generic write");
        }
        if ((mask & GenericExecute) != 0)
        {
            parts.Add("Generic execute");
        }

        if ((mask & StandardRightsAll) == StandardRightsAll)
        {
            parts.Add("Full control");
        }
        else
        {
            if ((mask & Delete) != 0)
            {
                parts.Add("Delete");
            }
            if ((mask & ReadControl) != 0)
            {
                parts.Add("Read control");
            }
            if ((mask & WriteDac) != 0)
            {
                parts.Add("Write DAC");
            }
            if ((mask & WriteOwner) != 0)
            {
                parts.Add("Write owner");
            }
            if ((mask & Synchronize) != 0)
            {
                parts.Add("Synchronize");
            }
        }

        return string.Join(", ", parts);
    }

    /// <summary>
    /// A handle type or an em-dash when blank (some handles carry no type).
    /// </summary>
    public static string HandleTypeText(string? type) => OrDash(type);

    /// <summary>
    /// A resolved handle name, or a calm placeholder when unresolved. The blank
    /// case is stated as "(unnamed)" rather than left empty so a row never looks
    /// like a rendering glitch; whether names were <em>restricted</em> is surfaced
    /// separately by <see cref="NamesLimitedNote"/>.
    /// </summary>
    public static string HandleNameText(string? name) =>
        string.IsNullOrWhiteSpace(name) ? "(unnamed)" : name!;

    /// <summary>
    /// A human byte size for a module image, auto-scaling B/KB/MB/GB with one
    /// decimal above bytes, e.g. 4096 → "4.0 KB", 0 → "—".
    /// </summary>
    public static string ByteSizeText(ulong bytes)
    {
        if (bytes == 0)
        {
            return "—";
        }
        if (bytes < 1024)
        {
            return string.Format(Inv, "{0} B", bytes);
        }
        double kb = bytes / 1024.0;
        if (kb < 1024)
        {
            return string.Format(Inv, "{0:0.#} KB", kb);
        }
        double mb = kb / 1024.0;
        if (mb < 1024)
        {
            return string.Format(Inv, "{0:0.#} MB", mb);
        }
        double gb = mb / 1024.0;
        return string.Format(Inv, "{0:0.#} GB", gb);
    }

    // ----------------------------------------------------------------------
    // Threads.
    // ----------------------------------------------------------------------

    /// <summary>A thread-state label, or "Unknown" when blank.</summary>
    public static string ThreadStateLabel(string? state) =>
        string.IsNullOrWhiteSpace(state) ? "Unknown" : state!;

    /// <summary>A wait-reason label, or an em-dash when blank / not waiting.</summary>
    public static string WaitReasonText(string? waitReason) => OrDash(waitReason);

    /// <summary>A CPU share from permille (0..1000) as a percent, e.g. 123 → "12.3%".</summary>
    public static string CpuPermilleText(uint permille) =>
        string.Format(Inv, "{0:0.#}%", permille / 10.0);

    /// <summary>
    /// A thread's combined CPU time (user + kernel) rendered as a compact
    /// duration. Inputs are FILETIME 100 ns ticks; sub-millisecond totals show as
    /// "0 ms".
    /// </summary>
    public static string CpuTimeText(long userTime100ns, long kernelTime100ns)
    {
        long total100ns = userTime100ns + kernelTime100ns;
        if (total100ns <= 0)
        {
            return "0 ms";
        }
        double ms = total100ns / 10_000.0;
        if (ms < 1000)
        {
            return string.Format(Inv, "{0:0} ms", ms);
        }
        double sec = ms / 1000.0;
        if (sec < 60)
        {
            return string.Format(Inv, "{0:0.0} s", sec);
        }
        long minutes = (long)(sec / 60);
        double remSec = sec - minutes * 60;
        return string.Format(Inv, "{0}m {1:0}s", minutes, remSec);
    }

    // ----------------------------------------------------------------------
    // Coverage notes (the honesty surface — task brief §2).
    // ----------------------------------------------------------------------

    /// <summary>
    /// The calm coverage note for a section whose reply set <c>limited</c>, or an
    /// empty string when coverage is full. Callers show the note <b>alongside</b>
    /// the partial data, never instead of it.
    /// </summary>
    public static string LimitedCoverageNote(bool limited) =>
        limited ? LimitedCoverageMessage : string.Empty;

    /// <summary>
    /// The coverage note for the Handles tab when name resolution was restricted
    /// (<c>names_limited</c>) — the handles are still listed; only some names
    /// could not be resolved without elevation. Empty when names were full.
    /// </summary>
    public static string NamesLimitedNote(bool namesLimited) =>
        namesLimited
            ? "Some handle names couldn't be resolved without an elevated Atlas — the handles themselves are still listed."
            : string.Empty;

    /// <summary>
    /// A friendly line for an <c>available = false</c> section, preferring the
    /// service's own <c>unavailable_reason</c> and falling back to a generic
    /// message. Used by Modules / resource-owner lookups that report availability.
    /// </summary>
    public static string UnavailableReason(string? reason, string fallback) =>
        string.IsNullOrWhiteSpace(reason) ? fallback : reason!;
}
