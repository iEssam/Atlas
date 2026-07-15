using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using Atlas.V0;

namespace Atlas.IpcClient;

/// <summary>
/// Pure mapping/formatting helpers for the signed-plugin registry surface
/// (PRD §18.3): signature labels + calm color tokens, the seven read-only
/// capability groups as friendly labels + one-line descriptions, a granted-caps
/// summary, and a one-line plugin summary. Free of I/O and of any WinUI type so
/// the view-models stay thin and the logic is unit-testable without a live server
/// (task brief §1).
///
/// <para>
/// Tone is the whole point of this surface. A plugin is a separate, read-only,
/// capability-scoped program that is off until enabled — the copy must make that
/// unmistakable <b>without</b> fear-mongering. So the signature tokens are calm:
/// a <em>signed</em> plugin reads as positive, an <em>unsigned</em> one as a
/// <em>caution</em> ("enable only if you trust the source") and never as a threat,
/// and an <em>unknown</em> verification is simply neutral. Only real danger earns
/// the red palette, and a plugin — read-only by construction — is not that.
/// </para>
/// </summary>
public static class PluginFormatter
{
    private static readonly CultureInfo Inv = CultureInfo.InvariantCulture;

    // ----------------------------------------------------------------------
    // Signature (PRD §18.3) — a calm, non-alarmist scale.
    // ----------------------------------------------------------------------

    /// <summary>
    /// A friendly signature label: "Signed" / "Unsigned" / "Unknown". "Unsigned"
    /// is a caution, not a verdict; "Unknown" means verification couldn't complete.
    /// </summary>
    public static string SignatureLabel(PluginSignature signature) => signature switch
    {
        PluginSignature.PluginSigned => "Signed",
        PluginSignature.PluginUnsigned => "Unsigned",
        PluginSignature.PluginSigUnknown => "Unknown",
        _ => "Unknown",
    };

    /// <summary>
    /// A calm color token for a signature badge: "signed" (positive), "unsigned"
    /// (caution — never the red danger palette), "unknown" (neutral). The consumer
    /// maps these to theme brushes; the point is that an unsigned plugin looks like
    /// something to weigh, not something to fear.
    /// </summary>
    public static string SignatureColorToken(PluginSignature signature) => signature switch
    {
        PluginSignature.PluginSigned => "signed",
        PluginSignature.PluginUnsigned => "unsigned",
        PluginSignature.PluginSigUnknown => "unknown",
        _ => "unknown",
    };

    /// <summary>A Segoe Fluent glyph for a signature badge (leading icon).</summary>
    public static string SignatureGlyph(PluginSignature signature) => signature switch
    {
        PluginSignature.PluginSigned => "",     // Shield / protected
        PluginSignature.PluginUnsigned => "",   // Caution
        PluginSignature.PluginSigUnknown => "", // Unknown / help
        _ => "",
    };

    /// <summary>
    /// A plain, calm one-line note for a signature, safe to show under the badge.
    /// The unsigned note is a caution that names the trade-off honestly rather than
    /// raising an alarm.
    /// </summary>
    public static string SignatureNote(PluginSignature signature) => signature switch
    {
        PluginSignature.PluginSigned =>
            "Signed — the publisher's identity was verified.",
        PluginSignature.PluginUnsigned =>
            "Unsigned — enable only if you trust the source.",
        PluginSignature.PluginSigUnknown =>
            "Signature couldn't be verified — treat it as unsigned unless you trust the source.",
        _ =>
            "Signature couldn't be verified — treat it as unsigned unless you trust the source.",
    };

    // ----------------------------------------------------------------------
    // Capabilities (PRD §18.3) — the seven read-only groups.
    // ----------------------------------------------------------------------

    /// <summary>The seven grantable read-only capability groups, in display order.</summary>
    public static IReadOnlyList<PluginCapability> AllCapabilities { get; } = new[]
    {
        PluginCapability.PluginCapSnapshot,
        PluginCapability.PluginCapHistory,
        PluginCapability.PluginCapSearch,
        PluginCapability.PluginCapIncidents,
        PluginCapability.PluginCapInventory,
        PluginCapability.PluginCapNetwork,
        PluginCapability.PluginCapForensics,
    };

    /// <summary>A short friendly label for a capability group.</summary>
    public static string CapabilityLabel(PluginCapability capability) => capability switch
    {
        PluginCapability.PluginCapSnapshot => "Live snapshot",
        PluginCapability.PluginCapHistory => "History",
        PluginCapability.PluginCapSearch => "Search",
        PluginCapability.PluginCapIncidents => "Incidents",
        PluginCapability.PluginCapInventory => "Inventory",
        PluginCapability.PluginCapNetwork => "Network",
        PluginCapability.PluginCapForensics => "Forensics",
        _ => "Unknown",
    };

    /// <summary>
    /// A one-line description of exactly what read-only slice a capability grants —
    /// so the user knows what they're allowing before they grant it. Each stresses
    /// that it is read-only.
    /// </summary>
    public static string CapabilityDescription(PluginCapability capability) => capability switch
    {
        PluginCapability.PluginCapSnapshot =>
            "Read the current snapshot of running processes and resource use.",
        PluginCapability.PluginCapHistory =>
            "Read historical metrics, events, and bookmarks over time.",
        PluginCapability.PluginCapSearch =>
            "Run read-only searches across processes, events, and bookmarks.",
        PluginCapability.PluginCapIncidents =>
            "Read detected incidents and their diagnoses.",
        PluginCapability.PluginCapInventory =>
            "Read the services, startup items, and scheduled tasks inventory.",
        PluginCapability.PluginCapNetwork =>
            "Read active connections and listening ports.",
        PluginCapability.PluginCapForensics =>
            "Read the system-change history and crash records.",
        _ => "An unrecognized capability.",
    };

    /// <summary>A Segoe Fluent glyph for a capability chip/row (leading icon).</summary>
    public static string CapabilityGlyph(PluginCapability capability) => capability switch
    {
        PluginCapability.PluginCapSnapshot => "",   // Activity
        PluginCapability.PluginCapHistory => "",    // History
        PluginCapability.PluginCapSearch => "",     // Search
        PluginCapability.PluginCapIncidents => "",  // Alert
        PluginCapability.PluginCapInventory => "",  // List / services
        PluginCapability.PluginCapNetwork => "",    // Network
        PluginCapability.PluginCapForensics => "",  // Timeline / records
        _ => "",
    };

    /// <summary>
    /// The distinct, display-ordered, meaningful (non-unspecified) capabilities from
    /// a granted set — de-duplicated and sorted into the canonical order so chips and
    /// summaries stay stable regardless of wire order.
    /// </summary>
    public static IReadOnlyList<PluginCapability> NormalizeCapabilities(
        IEnumerable<PluginCapability>? granted)
    {
        if (granted is null)
        {
            return System.Array.Empty<PluginCapability>();
        }
        var set = new HashSet<PluginCapability>(granted);
        return AllCapabilities.Where(set.Contains).ToList();
    }

    /// <summary>
    /// A one-line, plain-language summary of what a granted set allows, e.g.
    /// "Can read: Live snapshot, Network." Returns "No access granted — this plugin
    /// can't read anything." when the set is empty, which is the safe default.
    /// </summary>
    public static string GrantedSummary(IEnumerable<PluginCapability>? granted)
    {
        var caps = NormalizeCapabilities(granted);
        if (caps.Count == 0)
        {
            return "No access granted — this plugin can't read anything.";
        }
        return "Can read: " + string.Join(", ", caps.Select(CapabilityLabel)) + ".";
    }

    /// <summary>
    /// A compact granted-count phrase for a list row, e.g. "3 of 7 read-only
    /// capabilities" (singular "1 of 7 …"), or "No capabilities" when none.
    /// </summary>
    public static string GrantedCountText(IEnumerable<PluginCapability>? granted)
    {
        var caps = NormalizeCapabilities(granted);
        if (caps.Count == 0)
        {
            return "No capabilities";
        }
        return string.Format(
            Inv,
            "{0} of {1} read-only {2}",
            caps.Count,
            AllCapabilities.Count,
            caps.Count == 1 ? "capability" : "capabilities");
    }

    // ----------------------------------------------------------------------
    // Plugin one-liners.
    // ----------------------------------------------------------------------

    /// <summary>The plugin's display name, falling back to "(unnamed plugin)".</summary>
    public static string PluginName(Plugin plugin) =>
        plugin is null || string.IsNullOrWhiteSpace(plugin.Name) ? "(unnamed plugin)" : plugin.Name;

    /// <summary>
    /// A version phrase for a plugin, e.g. "v1.2.0", or empty when no version is set
    /// (so the UI can omit it cleanly).
    /// </summary>
    public static string VersionText(string? version) =>
        string.IsNullOrWhiteSpace(version) ? string.Empty : "v" + version.Trim();

    /// <summary>
    /// A publisher phrase for a plugin, e.g. "Contoso Ltd." When the signing
    /// certificate carried no subject (typically an unsigned plugin) this is
    /// "Unknown publisher" — a plain fact, not an alarm.
    /// </summary>
    public static string PublisherText(string? publisher) =>
        string.IsNullOrWhiteSpace(publisher) ? "Unknown publisher" : publisher.Trim();

    /// <summary>
    /// A one-line summary of a plugin for a compact row/tooltip, tying together its
    /// name, version, signature, enabled state, and granted breadth — e.g.
    /// "Timeline Insights v1.2.0 • Signed • On • 3 of 7 read-only capabilities".
    /// </summary>
    public static string PluginSummary(Plugin plugin)
    {
        if (plugin is null)
        {
            return "(unknown plugin)";
        }

        var parts = new List<string>();
        var head = PluginName(plugin);
        var ver = VersionText(plugin.Version);
        if (ver.Length > 0)
        {
            head += " " + ver;
        }
        parts.Add(head);
        parts.Add(SignatureLabel(plugin.Signature));
        parts.Add(plugin.Enabled ? "On" : "Off");
        parts.Add(GrantedCountText(plugin.Granted));
        return string.Join(" • ", parts);
    }
}
