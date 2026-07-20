using Atlas.V0;

namespace Atlas.IpcClient;

/// <summary>
/// Consolidates ConsentStore's implementation-level rows into one user-facing
/// app summary per capability. Desktop applications can legitimately have
/// several records even though the user recognizes them as one executable.
/// </summary>
public static class PrivacyUsageAggregator
{
    public static IReadOnlyList<PrivacyUsageSummary> Aggregate(IEnumerable<PrivacyUsage> usages)
    {
        var aggregates = new Dictionary<PrivacyUsageKey, MutableSummary>();
        foreach (var usage in usages)
        {
            var displayName = string.IsNullOrWhiteSpace(usage.DisplayName)
                ? (string.IsNullOrWhiteSpace(usage.AppId) ? "(unknown app)" : usage.AppId)
                : usage.DisplayName;
            displayName = displayName.Trim();
            var key = new PrivacyUsageKey(
                usage.Capability,
                displayName.ToUpperInvariant(),
                usage.Packaged);
            if (!aggregates.TryGetValue(key, out var summary))
            {
                summary = new MutableSummary(displayName, usage.Packaged);
                aggregates.Add(key, summary);
            }

            summary.InUse |= usage.InUse;
            summary.LastStartMs = Math.Max(summary.LastStartMs, usage.LastStartMs);
            summary.LastStopMs = Math.Max(summary.LastStopMs, usage.LastStopMs);
            summary.RecordCount++;
        }

        return aggregates
            .Select(entry => new PrivacyUsageSummary(
                entry.Key.Capability,
                entry.Value.DisplayName,
                entry.Value.Packaged,
                entry.Value.InUse,
                entry.Value.LastStartMs,
                entry.Value.LastStopMs,
                entry.Value.RecordCount))
            .ToArray();
    }

    private readonly record struct PrivacyUsageKey(
        CapabilityKind Capability,
        string NormalizedDisplayName,
        bool Packaged);

    private sealed class MutableSummary(string displayName, bool packaged)
    {
        public string DisplayName { get; } = displayName;
        public bool Packaged { get; } = packaged;
        public bool InUse { get; set; }
        public long LastStartMs { get; set; }
        public long LastStopMs { get; set; }
        public int RecordCount { get; set; }
    }
}

public sealed record PrivacyUsageSummary(
    CapabilityKind Capability,
    string DisplayName,
    bool Packaged,
    bool InUse,
    long LastStartMs,
    long LastStopMs,
    int RecordCount);
