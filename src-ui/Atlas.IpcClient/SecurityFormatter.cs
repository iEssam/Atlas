using System.Globalization;
using System.Text;

namespace Atlas.IpcClient;

/// <summary>
/// Pure mapping/formatting helpers for the R3 expert security surface — the
/// Inspector's Security tab (PRD §9.4.1/§9.4.6): the file hash, signing
/// certificate chain, token privileges/groups/capabilities, and process
/// mitigation policies. Free of I/O and of any WinUI type so the view-model stays
/// thin and the logic is unit-testable without a live server (task brief §1).
///
/// <para>
/// Tone is the whole point here, exactly as with <see cref="R2Formatter"/>. This
/// is <b>expert data shown factually</b>: a held privilege, an app-container
/// sandbox, a blank field, or an <c>Unsigned</c> binary is <b>information, not an
/// accusation</b> (PRD §9.6.7 honesty principle). A privilege being present is
/// normal, so its enabled/available state stays neutral-informational and never
/// borrows an alarm color; the signature badge reuses
/// <see cref="R2Formatter.SignatureTrustToken"/> so "unsigned" is a calm
/// <em>caution</em>, never the red reserved for genuine danger; and the coverage
/// note frames a limited view as an elevation gap, not something the process is
/// hiding.
/// </para>
/// </summary>
public static class SecurityFormatter
{
    private static readonly CultureInfo Inv = CultureInfo.InvariantCulture;

    /// <summary>
    /// The calm coverage note shown when the security reply set
    /// <c>metadata.limited</c> — some fields needed an elevated Atlas. Framed as an
    /// elevation limitation, never as something suspicious about the process (task
    /// brief §2). Callers show it <b>alongside</b> the partial data, never instead.
    /// </summary>
    public const string LimitedCoverageMessage =
        "Some security details need an elevated Atlas — run as administrator for full coverage.";

    /// <summary>The coverage note for a limited reply, or an empty string when full.</summary>
    public static string LimitedCoverageNote(bool limited) =>
        limited ? LimitedCoverageMessage : string.Empty;

    // ----------------------------------------------------------------------
    // File hash (SHA-256).
    // ----------------------------------------------------------------------

    /// <summary>
    /// A SHA-256 hex digest grouped into space-separated 8-character blocks for
    /// legibility, e.g. "e3b0c442 98fc1c14 …". Casing is preserved as given (the
    /// service emits lowercase). A blank hash renders as an em-dash rather than an
    /// empty string so the row never looks like a glitch; whitespace and any
    /// "sha256:" prefix are tolerated. Non-hex input is passed through grouped
    /// as-is rather than rejected.
    /// </summary>
    public static string Sha256Grouped(string? sha)
    {
        var s = Normalize(sha);
        if (s.Length == 0)
        {
            return "—";
        }
        var sb = new StringBuilder(s.Length + s.Length / 8);
        for (int i = 0; i < s.Length; i++)
        {
            if (i > 0 && i % 8 == 0)
            {
                sb.Append(' ');
            }
            sb.Append(s[i]);
        }
        return sb.ToString();
    }

    /// <summary>
    /// A short SHA-256 form for a dense header — the first and last eight hex
    /// characters joined by an ellipsis, e.g. "e3b0c442…7852b855". Short digests
    /// (16 chars or fewer) are returned whole; a blank hash renders as an em-dash.
    /// </summary>
    public static string Sha256Short(string? sha)
    {
        var s = Normalize(sha);
        if (s.Length == 0)
        {
            return "—";
        }
        if (s.Length <= 16)
        {
            return s;
        }
        return string.Concat(s.AsSpan(0, 8), "…", s.AsSpan(s.Length - 8, 8));
    }

    /// <summary>
    /// The raw digest suitable for copy-to-clipboard (whitespace/prefix stripped),
    /// or an empty string when blank — callers guard the copy affordance on this.
    /// </summary>
    public static string Sha256Raw(string? sha) => Normalize(sha);

    // ----------------------------------------------------------------------
    // Certificate chain.
    // ----------------------------------------------------------------------

    /// <summary>
    /// A SHA-1 certificate thumbprint grouped into space-separated 4-character
    /// blocks and upper-cased (the familiar certmgr rendering), e.g.
    /// "A1B2 C3D4 …". Whitespace is tolerated; a blank thumbprint renders as an
    /// em-dash.
    /// </summary>
    public static string ThumbprintGrouped(string? thumbprint)
    {
        var s = Normalize(thumbprint).ToUpperInvariant();
        if (s.Length == 0)
        {
            return "—";
        }
        var sb = new StringBuilder(s.Length + s.Length / 4);
        for (int i = 0; i < s.Length; i++)
        {
            if (i > 0 && i % 4 == 0)
            {
                sb.Append(' ');
            }
            sb.Append(s[i]);
        }
        return sb.ToString();
    }

    /// <summary>
    /// A certificate's validity window as a calm one-liner keyed on the expiry, e.g.
    /// "valid until 2027-01-02". A non-positive <paramref name="notAfterMs"/> (the
    /// service couldn't read it) renders as an em-dash.
    /// </summary>
    public static string CertValidUntil(long notAfterMs)
    {
        if (notAfterMs <= 0)
        {
            return "—";
        }
        try
        {
            return "valid until " + DateTimeOffset.FromUnixTimeMilliseconds(notAfterMs)
                .LocalDateTime.ToString("yyyy-MM-dd", Inv);
        }
        catch
        {
            return "—";
        }
    }

    /// <summary>
    /// A factual note about a certificate's validity window relative to
    /// <paramref name="nowMs"/> (injected for testability): "expired 2020-01-01"
    /// when past its expiry, "expires soon (2026-08-01)" when it lapses within
    /// <see cref="NearExpiryWindowDays"/>, "not yet valid" when its start is in the
    /// future, and an empty string in the normal case. This is stated plainly — an
    /// expired signing certificate is common and not evidence of wrongdoing; the
    /// note simply lets an expert see it (task brief §1).
    /// </summary>
    public static string CertValidityNote(long notBeforeMs, long notAfterMs, long nowMs)
    {
        if (notAfterMs > 0 && nowMs > notAfterMs)
        {
            return "expired " + ToDay(notAfterMs);
        }
        if (notBeforeMs > 0 && nowMs < notBeforeMs)
        {
            return "not yet valid";
        }
        if (notAfterMs > 0)
        {
            long windowMs = (long)NearExpiryWindowDays * 24 * 60 * 60 * 1000;
            if (notAfterMs - nowMs <= windowMs)
            {
                return "expires soon (" + ToDay(notAfterMs) + ")";
            }
        }
        return string.Empty;
    }

    /// <summary>The near-expiry window (days) that <see cref="CertValidityNote"/> flags.</summary>
    public const int NearExpiryWindowDays = 30;

    /// <summary>
    /// A calm color token for a certificate's validity note: "expired" (past
    /// expiry), "caution" (expiring soon, or not yet valid), or "ok" (normal).
    /// Deliberately tops out at caution — an out-of-window signing certificate is a
    /// fact to surface, not an alarm (task brief §1). Consumers may map "expired"
    /// and "caution" to the same amber; the distinction is for callers that want it.
    /// </summary>
    public static string CertValidityToken(long notBeforeMs, long notAfterMs, long nowMs)
    {
        if (notAfterMs > 0 && nowMs > notAfterMs)
        {
            return "expired";
        }
        if (notBeforeMs > 0 && nowMs < notBeforeMs)
        {
            return "caution";
        }
        if (notAfterMs > 0)
        {
            long windowMs = (long)NearExpiryWindowDays * 24 * 60 * 60 * 1000;
            if (notAfterMs - nowMs <= windowMs)
            {
                return "caution";
            }
        }
        return "ok";
    }

    /// <summary>A certificate subject/issuer field, or a calm placeholder when blank.</summary>
    public static string CertNameText(string? name) =>
        string.IsNullOrWhiteSpace(name) ? "(unknown)" : name!.Trim();

    // ----------------------------------------------------------------------
    // Token — privileges.
    // ----------------------------------------------------------------------

    /// <summary>
    /// A friendly gloss for a well-known Windows privilege constant (the
    /// <c>SeXxxPrivilege</c> name is always kept by the caller and shown alongside),
    /// e.g. SeDebugPrivilege → "Debug programs". Returns an empty string for an
    /// unrecognized name — the raw <c>SeXxx</c> name then stands on its own. The
    /// gloss is descriptive, never a judgement: holding a privilege is normal.
    /// </summary>
    public static string PrivilegeGloss(string? name)
    {
        if (string.IsNullOrWhiteSpace(name))
        {
            return string.Empty;
        }
        return name.Trim() switch
        {
            "SeDebugPrivilege" => "Debug programs",
            "SeBackupPrivilege" => "Back up files and directories",
            "SeRestorePrivilege" => "Restore files and directories",
            "SeShutdownPrivilege" => "Shut down the system",
            "SeRemoteShutdownPrivilege" => "Force shutdown from a remote system",
            "SeTakeOwnershipPrivilege" => "Take ownership of files or objects",
            "SeLoadDriverPrivilege" => "Load and unload device drivers",
            "SeSystemtimePrivilege" => "Change the system time",
            "SeTimeZonePrivilege" => "Change the time zone",
            "SeSecurityPrivilege" => "Manage auditing and the security log",
            "SeIncreaseBasePriorityPrivilege" => "Raise scheduling priority",
            "SeIncreaseQuotaPrivilege" => "Adjust memory quotas for a process",
            "SeIncreaseWorkingSetPrivilege" => "Increase a process working set",
            "SeCreatePagefilePrivilege" => "Create a pagefile",
            "SeCreateGlobalPrivilege" => "Create global objects",
            "SeCreateSymbolicLinkPrivilege" => "Create symbolic links",
            "SeAssignPrimaryTokenPrivilege" => "Replace a process-level token",
            "SeImpersonatePrivilege" => "Impersonate a client after authentication",
            "SeManageVolumePrivilege" => "Perform volume maintenance tasks",
            "SeProfileSingleProcessPrivilege" => "Profile a single process",
            "SeSystemProfilePrivilege" => "Profile system performance",
            "SeChangeNotifyPrivilege" => "Bypass traverse checking",
            "SeUndockPrivilege" => "Remove the computer from a docking station",
            "SeSystemEnvironmentPrivilege" => "Modify firmware environment values",
            "SeAuditPrivilege" => "Generate security audits",
            "SeTcbPrivilege" => "Act as part of the operating system",
            "SeDelegateSessionUserImpersonatePrivilege" => "Impersonate other users' sessions",
            "SeTrustedCredManAccessPrivilege" => "Access Credential Manager as a trusted caller",
            "SeRelabelPrivilege" => "Modify an object label",
            "SeLockMemoryPrivilege" => "Lock pages in memory",
            "SeMachineAccountPrivilege" => "Add workstations to the domain",
            "SeEnableDelegationPrivilege" => "Enable computer and user accounts to be trusted for delegation",
            "SeSyncAgentPrivilege" => "Synchronize directory service data",
            _ => string.Empty,
        };
    }

    /// <summary>
    /// A neutral state token for a privilege: "enabled" when active, "available"
    /// when held-but-inactive. <b>Neither is alarmist</b> — a privilege being
    /// present, enabled or not, is a normal part of a process's token; the consumer
    /// maps both to calm, informational colors, never a danger palette (task
    /// brief §1).
    /// </summary>
    public static string PrivilegeStateToken(bool enabled) => enabled ? "enabled" : "available";

    /// <summary>A short caption for a privilege's state: "Enabled" / "Available".</summary>
    public static string PrivilegeStateLabel(bool enabled) => enabled ? "Enabled" : "Available";

    /// <summary>A privilege name, or a calm placeholder when blank (so a row never renders empty).</summary>
    public static string PrivilegeNameText(string? name) =>
        string.IsNullOrWhiteSpace(name) ? "(unnamed privilege)" : name!.Trim();

    // ----------------------------------------------------------------------
    // Token — identity / sandbox.
    // ----------------------------------------------------------------------

    /// <summary>
    /// The app-container (sandbox) state as a plain fact: "App container (sandboxed)"
    /// when true, "Not app-contained" when false. Neither framing implies a problem —
    /// most desktop apps are not app-contained, and that is unremarkable.
    /// </summary>
    public static string AppContainerLabel(bool appContainer) =>
        appContainer ? "App container (sandboxed)" : "Not app-contained";

    /// <summary>A group name / SID, or a calm placeholder when blank.</summary>
    public static string GroupText(string? group) =>
        string.IsNullOrWhiteSpace(group) ? "(unnamed group)" : group!.Trim();

    /// <summary>An app-container capability name, or a calm placeholder when blank.</summary>
    public static string CapabilityText(string? capability) =>
        string.IsNullOrWhiteSpace(capability) ? "(unnamed capability)" : capability!.Trim();

    // ----------------------------------------------------------------------
    // Mitigations.
    // ----------------------------------------------------------------------

    /// <summary>
    /// A process-mitigation label passthrough (e.g. "DEP", "ASLR (high-entropy)",
    /// "CFG", "no child processes"), trimmed. A mitigation being <b>on</b> is a
    /// hardening the process opted into — shown as a neutral chip, never a warning.
    /// A blank entry becomes an em-dash but is normally filtered out by the caller.
    /// </summary>
    public static string MitigationLabel(string? mitigation) =>
        string.IsNullOrWhiteSpace(mitigation) ? "—" : mitigation!.Trim();

    // ----------------------------------------------------------------------

    private static string Normalize(string? value)
    {
        if (string.IsNullOrWhiteSpace(value))
        {
            return string.Empty;
        }
        var s = value.Trim();
        // Tolerate an algorithm prefix like "sha256:" or "SHA1:".
        int colon = s.IndexOf(':');
        if (colon >= 0 && colon <= 8)
        {
            s = s[(colon + 1)..].Trim();
        }
        // Drop internal whitespace so grouping is deterministic.
        if (s.IndexOf(' ') >= 0 || s.IndexOf('\t') >= 0)
        {
            var sb = new StringBuilder(s.Length);
            foreach (var ch in s)
            {
                if (!char.IsWhiteSpace(ch))
                {
                    sb.Append(ch);
                }
            }
            s = sb.ToString();
        }
        return s;
    }

    private static string ToDay(long ms)
    {
        try
        {
            return DateTimeOffset.FromUnixTimeMilliseconds(ms).LocalDateTime.ToString("yyyy-MM-dd", Inv);
        }
        catch
        {
            return "—";
        }
    }
}
