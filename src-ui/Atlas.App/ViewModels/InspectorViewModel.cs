using System;
using System.Collections.ObjectModel;
using System.Globalization;
using System.Threading;
using System.Threading.Tasks;
using Atlas.IpcClient;
using Atlas.V0;
using CommunityToolkit.Mvvm.ComponentModel;
using Microsoft.UI.Dispatching;

namespace Atlas.App.ViewModels;

/// <summary>
/// Drives the Process Inspector (R2, PRD §9.4) — the milestone centerpiece. It
/// holds the state for five lazily-loaded tabs (Overview, Handles, Modules,
/// Threads, Security), each backed by one on-demand read-only RPC. Tabs load on
/// first view and expose a per-tab refresh; nothing is fetched upfront (task
/// brief §2). The Security tab (R3, PRD §9.4.1/§9.4.6) adds expert detail — the
/// signing certificate chain, file hash, token privileges/groups/capabilities,
/// and process mitigation policies — shown factually with the same coverage
/// honesty.
///
/// <para>
/// Coverage is surfaced <b>honestly</b> (task brief §2): when a reply sets
/// <c>limited</c> / <c>names_limited</c> or reports <c>available = false</c>, the
/// VM raises a calm note <em>alongside</em> whatever partial data came back — it
/// never hides the data and never implies the process is suspicious because a
/// field is blank. Against a service too old to serve these RPCs (Unimplemented →
/// Unsupported) each tab shows a "server too old" state instead of crashing.
/// </para>
/// </summary>
public sealed partial class InspectorViewModel : ObservableObject
{
    private static readonly CultureInfo Inv = CultureInfo.InvariantCulture;

    private readonly DispatcherQueue _dispatcher;
    private readonly string? _who;
    private readonly uint _pid;
    private readonly long _createTime100ns;

    private bool _overviewLoaded;
    private bool _handlesLoaded;
    private bool _modulesLoaded;
    private bool _threadsLoaded;
    private bool _securityLoaded;

    public uint Pid => _pid;
    public long CreateTime100ns => _createTime100ns;

    /// <summary>Window title, e.g. "chrome.exe (pid 4242) — Inspector".</summary>
    public string Title { get; }

    /// <summary>Header line under the title: image name + pid.</summary>
    public string HeaderText { get; }

    public InspectorViewModel(
        DispatcherQueue dispatcher, string? who, uint pid, long createTime100ns, string imageName)
    {
        _dispatcher = dispatcher;
        _who = who;
        _pid = pid;
        _createTime100ns = createTime100ns;

        var name = string.IsNullOrWhiteSpace(imageName) ? $"pid {pid}" : imageName;
        Title = $"{name} (pid {pid}) — Inspector";
        HeaderText = $"{name}  •  pid {pid}";
        OverviewName = name;
    }

    // ======================================================================
    // Overview tab (GetProcessDetail).
    // ======================================================================

    [ObservableProperty] private bool _overviewLoading;
    [ObservableProperty] private bool _overviewUnavailable;
    [ObservableProperty] private string _overviewUnavailableText = string.Empty;
    [ObservableProperty] private bool _overviewHasData;
    [ObservableProperty] private string _overviewLimitedNote = string.Empty;
    [ObservableProperty] private bool _overviewIsLimited;

    [ObservableProperty] private string _overviewName = string.Empty;
    [ObservableProperty] private string _overviewPath = "—";
    [ObservableProperty] private string _overviewPid = string.Empty;
    [ObservableProperty] private string _overviewParentPid = string.Empty;
    [ObservableProperty] private string _overviewCommandLine = "—";
    [ObservableProperty] private string _overviewWorkingDir = "—";
    [ObservableProperty] private string _overviewUser = "—";
    [ObservableProperty] private string _overviewIntegrity = "—";
    [ObservableProperty] private string _overviewElevation = string.Empty;
    [ObservableProperty] private string _overviewArchitecture = "—";
    [ObservableProperty] private string _overviewSignature = "—";
    [ObservableProperty] private string _overviewSignatureToken = "unknown";
    [ObservableProperty] private string _overviewPublisher = "—";
    [ObservableProperty] private string _overviewPackage = "—";
    [ObservableProperty] private string _overviewFileVersion = "—";
    [ObservableProperty] private string _overviewProductName = "—";
    [ObservableProperty] private string _overviewStartTime = "—";
    [ObservableProperty] private string _overviewCounts = "—";
    [ObservableProperty] private string _overviewGpuUsage = "No measured GPU use";
    [ObservableProperty] private string _overviewGpuMemory = "0 MB";

    /// <summary>Loads the Overview tab once (no-op if already loaded).</summary>
    public Task EnsureOverviewAsync()
    {
        if (_overviewLoaded)
        {
            return Task.CompletedTask;
        }
        _overviewLoaded = true;
        return RefreshOverviewAsync();
    }

    public async Task RefreshOverviewAsync()
    {
        OverviewLoading = true;
        OverviewUnavailable = false;
        OverviewHasData = false;
        OverviewLimitedNote = string.Empty;

        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.GetProcessDetailAsync(_pid, _createTime100ns).ConfigureAwait(false);
            ProcessRow? liveGpu = null;
            if (outcome.Supported && outcome.Value.Available)
            {
                var snapshot = await channel.GetSnapshotAsync(0).ConfigureAwait(false);
                liveGpu = snapshot.Processes.FirstOrDefault(p =>
                    p.Pid == _pid && (_createTime100ns == 0 || p.CreateTime100Ns == _createTime100ns));
            }

            Post(() =>
            {
                OverviewLoading = false;

                if (!outcome.Supported)
                {
                    OverviewUnavailable = true;
                    OverviewUnavailableText =
                        "Process detail is unavailable — the connected service is too old.";
                    return;
                }

                var reply = outcome.Value;
                if (!reply.Available)
                {
                    OverviewUnavailable = true;
                    OverviewUnavailableText = R2Formatter.UnavailableReason(
                        reply.UnavailableReason, "This process is no longer available.");
                    return;
                }

                ApplyDetail(reply.Detail);
                ApplyGpu(liveGpu);
                OverviewHasData = true;
            });
        }
        catch (Exception ex)
        {
            Post(() =>
            {
                OverviewLoading = false;
                OverviewUnavailable = true;
                OverviewUnavailableText = $"Could not reach the service: {ex.Message}";
            });
        }
    }

    private void ApplyDetail(ProcessDetail d)
    {
        OverviewName = string.IsNullOrWhiteSpace(d.ImageName) ? OverviewName : d.ImageName;
        OverviewPath = R2Formatter.OrDash(d.ImagePath);
        OverviewPid = d.Pid.ToString(Inv);
        OverviewParentPid = d.ParentPid.ToString(Inv);
        OverviewCommandLine = R2Formatter.OrDash(d.CommandLine);
        OverviewWorkingDir = R2Formatter.OrDash(d.WorkingDirectory);
        OverviewUser = R2Formatter.UserText(d.UserName, d.UserSid);
        OverviewIntegrity = R2Formatter.IntegrityLabel(d.IntegrityLevel);
        OverviewElevation = R2Formatter.ElevationLabel(d.Elevated);
        OverviewArchitecture = R2Formatter.ArchitectureLabel(d.Architecture);
        OverviewSignature = R2Formatter.SignatureStatusLabel(d.SignatureStatus);
        OverviewSignatureToken = R2Formatter.SignatureTrustToken(d.SignatureStatus);
        OverviewPublisher = R2Formatter.PublisherText(d.Publisher);
        OverviewPackage = R2Formatter.PackageText(d.PackageIdentity);
        OverviewFileVersion = R2Formatter.OrDash(d.FileVersion);
        OverviewProductName = R2Formatter.OrDash(d.ProductName);
        OverviewStartTime = FormatEpochMs(d.StartTimeMs);
        OverviewCounts = string.Format(
            Inv, "{0} threads  •  {1} handles", d.ThreadCount, d.HandleCount);

        OverviewIsLimited = d.Limited;
        OverviewLimitedNote = R2Formatter.LimitedCoverageNote(d.Limited);
    }

    private void ApplyGpu(ProcessRow? p)
    {
        if (p is null)
        {
            OverviewGpuUsage = "No current GPU sample for this process";
            OverviewGpuMemory = "No current GPU memory sample";
            return;
        }
        OverviewGpuUsage = $"{p.GpuPermille / 10.0:F1} % (busiest engine)";
        OverviewGpuMemory = $"{p.GpuDedicatedBytes / 1048576.0:F0} MB dedicated  •  {p.GpuSharedBytes / 1048576.0:F0} MB shared";
    }

    // ======================================================================
    // Handles tab (ListHandles).
    // ======================================================================

    public ObservableCollection<HandleRowItem> Handles { get; } = new();

    [ObservableProperty] private bool _handlesLoading;
    [ObservableProperty] private bool _handlesUnavailable;
    [ObservableProperty] private string _handlesUnavailableText = string.Empty;
    [ObservableProperty] private bool _handlesEmpty;
    [ObservableProperty] private string _handlesStatus = string.Empty;
    [ObservableProperty] private string _handlesNamesLimitedNote = string.Empty;
    [ObservableProperty] private bool _handlesNamesLimited;
    [ObservableProperty] private string _handleTypeFilter = string.Empty;

    private CancellationTokenSource? _handlesFilterCts;
    private static readonly TimeSpan FilterDebounce = TimeSpan.FromMilliseconds(350);

    public Task EnsureHandlesAsync()
    {
        if (_handlesLoaded)
        {
            return Task.CompletedTask;
        }
        _handlesLoaded = true;
        return RefreshHandlesAsync();
    }

    partial void OnHandleTypeFilterChanged(string value)
    {
        if (!_handlesLoaded)
        {
            return; // don't fetch before the tab is first shown
        }
        _handlesFilterCts?.Cancel();
        var cts = new CancellationTokenSource();
        _handlesFilterCts = cts;
        _ = DebouncedRefreshHandlesAsync(cts.Token);
    }

    private async Task DebouncedRefreshHandlesAsync(CancellationToken ct)
    {
        try
        {
            await Task.Delay(FilterDebounce, ct).ConfigureAwait(false);
        }
        catch (OperationCanceledException)
        {
            return;
        }
        if (!ct.IsCancellationRequested)
        {
            await RefreshHandlesAsync().ConfigureAwait(false);
        }
    }

    public async Task RefreshHandlesAsync()
    {
        var filter = HandleTypeFilter?.Trim() ?? string.Empty;

        HandlesLoading = true;
        HandlesUnavailable = false;
        HandlesEmpty = false;
        HandlesNamesLimited = false;
        HandlesNamesLimitedNote = string.Empty;
        HandlesStatus = "Loading…";

        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.ListHandlesAsync(_pid, filter).ConfigureAwait(false);

            Post(() =>
            {
                HandlesLoading = false;
                Handles.Clear();

                if (!outcome.Supported)
                {
                    HandlesUnavailable = true;
                    HandlesUnavailableText = "Handles are unavailable — the connected service is too old.";
                    HandlesStatus = string.Empty;
                    return;
                }

                var reply = outcome.Value;
                foreach (var h in reply.Handles)
                {
                    Handles.Add(new HandleRowItem(
                        R2Formatter.HandleTypeText(h.Type),
                        R2Formatter.HandleNameText(h.Name),
                        R2Formatter.HandleText(h.Handle),
                        FormatAccess(h.GrantedAccess)));
                }

                HandlesNamesLimited = reply.NamesLimited;
                HandlesNamesLimitedNote = R2Formatter.NamesLimitedNote(reply.NamesLimited);
                HandlesEmpty = Handles.Count == 0;

                var count = $"{Handles.Count} handle{(Handles.Count == 1 ? "" : "s")}";
                if (reply.Truncated)
                {
                    count += " (list truncated — showing the first results)";
                }
                if (filter.Length > 0)
                {
                    count += $" • type “{filter}”";
                }
                HandlesStatus = Handles.Count == 0
                    ? (filter.Length == 0 ? "No handles reported." : $"No “{filter}” handles.")
                    : count;
            });
        }
        catch (Exception ex)
        {
            Post(() =>
            {
                HandlesLoading = false;
                Handles.Clear();
                HandlesUnavailable = true;
                HandlesUnavailableText = $"Could not reach the service: {ex.Message}";
                HandlesStatus = string.Empty;
            });
        }
    }

    private static string FormatAccess(uint mask)
    {
        var hex = R2Formatter.GrantedAccessText(mask);
        var summary = R2Formatter.AccessRightsSummary(mask);
        return summary.Length == 0 ? hex : $"{hex}  ({summary})";
    }

    // ======================================================================
    // Modules tab (ListModules).
    // ======================================================================

    public ObservableCollection<ModuleRowItem> Modules { get; } = new();

    [ObservableProperty] private bool _modulesLoading;
    [ObservableProperty] private bool _modulesUnavailable;
    [ObservableProperty] private string _modulesUnavailableText = string.Empty;
    [ObservableProperty] private bool _modulesEmpty;
    [ObservableProperty] private string _modulesStatus = string.Empty;

    public Task EnsureModulesAsync()
    {
        if (_modulesLoaded)
        {
            return Task.CompletedTask;
        }
        _modulesLoaded = true;
        return RefreshModulesAsync();
    }

    public async Task RefreshModulesAsync()
    {
        ModulesLoading = true;
        ModulesUnavailable = false;
        ModulesEmpty = false;
        ModulesStatus = "Loading…";

        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.ListModulesAsync(_pid).ConfigureAwait(false);

            Post(() =>
            {
                ModulesLoading = false;
                Modules.Clear();

                if (!outcome.Supported)
                {
                    ModulesUnavailable = true;
                    ModulesUnavailableText = "Modules are unavailable — the connected service is too old.";
                    ModulesStatus = string.Empty;
                    return;
                }

                var reply = outcome.Value;
                if (!reply.Available)
                {
                    ModulesUnavailable = true;
                    ModulesUnavailableText = R2Formatter.UnavailableReason(
                        reply.UnavailableReason,
                        "Modules couldn't be read for this process (elevation may help).");
                    ModulesStatus = string.Empty;
                    return;
                }

                foreach (var m in reply.Modules)
                {
                    Modules.Add(new ModuleRowItem(
                        R2Formatter.OrDash(m.Name),
                        R2Formatter.OrDash(m.Path),
                        R2Formatter.OrDash(m.Version),
                        R2Formatter.PublisherText(m.Publisher),
                        m.Signed ? "Signed" : "Unsigned",
                        m.Signed ? "signed" : "caution"));
                }

                ModulesEmpty = Modules.Count == 0;
                ModulesStatus = Modules.Count == 0
                    ? "No modules reported."
                    : $"{Modules.Count} module{(Modules.Count == 1 ? "" : "s")}";
            });
        }
        catch (Exception ex)
        {
            Post(() =>
            {
                ModulesLoading = false;
                Modules.Clear();
                ModulesUnavailable = true;
                ModulesUnavailableText = $"Could not reach the service: {ex.Message}";
                ModulesStatus = string.Empty;
            });
        }
    }

    // ======================================================================
    // Threads tab (ListThreads).
    // ======================================================================

    public ObservableCollection<ThreadRowItem> Threads { get; } = new();

    [ObservableProperty] private bool _threadsLoading;
    [ObservableProperty] private bool _threadsUnavailable;
    [ObservableProperty] private string _threadsUnavailableText = string.Empty;
    [ObservableProperty] private bool _threadsEmpty;
    [ObservableProperty] private string _threadsStatus = string.Empty;

    public Task EnsureThreadsAsync()
    {
        if (_threadsLoaded)
        {
            return Task.CompletedTask;
        }
        _threadsLoaded = true;
        return RefreshThreadsAsync();
    }

    public async Task RefreshThreadsAsync()
    {
        ThreadsLoading = true;
        ThreadsUnavailable = false;
        ThreadsEmpty = false;
        ThreadsStatus = "Loading…";

        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.ListThreadsAsync(_pid).ConfigureAwait(false);

            Post(() =>
            {
                ThreadsLoading = false;
                Threads.Clear();

                if (!outcome.Supported)
                {
                    ThreadsUnavailable = true;
                    ThreadsUnavailableText = "Threads are unavailable — the connected service is too old.";
                    ThreadsStatus = string.Empty;
                    return;
                }

                foreach (var t in outcome.Value.Threads)
                {
                    Threads.Add(new ThreadRowItem(
                        t.Tid.ToString(Inv),
                        R2Formatter.AddressText(t.StartAddress),
                        R2Formatter.ThreadStateLabel(t.State),
                        R2Formatter.WaitReasonText(t.WaitReason),
                        t.Priority.ToString(Inv),
                        R2Formatter.CpuPermilleText(t.CpuPermille),
                        R2Formatter.CpuTimeText(t.UserTime100Ns, t.KernelTime100Ns)));
                }

                ThreadsEmpty = Threads.Count == 0;
                ThreadsStatus = Threads.Count == 0
                    ? "No threads reported."
                    : $"{Threads.Count} thread{(Threads.Count == 1 ? "" : "s")}";
            });
        }
        catch (Exception ex)
        {
            Post(() =>
            {
                ThreadsLoading = false;
                Threads.Clear();
                ThreadsUnavailable = true;
                ThreadsUnavailableText = $"Could not reach the service: {ex.Message}";
                ThreadsStatus = string.Empty;
            });
        }
    }

    // ======================================================================
    // Security tab (GetSecurityMetadata, R3 / PRD §9.4.1/§9.4.6).
    // ======================================================================
    //
    // Expert security detail shown FACTUALLY: signing certificate chain, file
    // hash, token privileges/groups/capabilities, and process mitigation
    // policies. Coverage is surfaced honestly exactly like the other tabs — a
    // reply's available=false shows a calm "server too old" / "process exited"
    // state, and metadata.limited raises a calm InfoBar alongside whatever partial
    // data came back. A held privilege, a blank field, or an unsigned binary is
    // information, never an accusation (task brief §2).

    public ObservableCollection<CertRowItem> CertChain { get; } = new();
    public ObservableCollection<PrivilegeRowItem> Privileges { get; } = new();
    public ObservableCollection<string> Groups { get; } = new();
    public ObservableCollection<string> Capabilities { get; } = new();
    public ObservableCollection<string> Mitigations { get; } = new();

    [ObservableProperty] private bool _securityLoading;
    [ObservableProperty] private bool _securityUnavailable;
    [ObservableProperty] private string _securityUnavailableText = string.Empty;
    [ObservableProperty] private bool _securityHasData;
    [ObservableProperty] private bool _securityIsLimited;
    [ObservableProperty] private string _securityLimitedNote = string.Empty;

    // Signature.
    [ObservableProperty] private string _securitySignature = "—";
    [ObservableProperty] private string _securitySignatureToken = "unknown";
    [ObservableProperty] private bool _securityCertChainEmpty;

    // File.
    [ObservableProperty] private string _securitySha256Grouped = "—";
    [ObservableProperty] private string _securitySha256Raw = string.Empty;
    [ObservableProperty] private bool _securityHasSha256;

    // Token.
    [ObservableProperty] private string _securityIntegrity = "—";
    [ObservableProperty] private string _securityElevation = "—";
    [ObservableProperty] private string _securityAppContainer = "—";
    [ObservableProperty] private string _securityUserSid = "—";
    [ObservableProperty] private bool _securityPrivilegesEmpty;
    [ObservableProperty] private bool _securityGroupsEmpty;
    [ObservableProperty] private bool _securityCapabilitiesEmpty;

    // Mitigations.
    [ObservableProperty] private bool _securityMitigationsEmpty;

    public Task EnsureSecurityAsync()
    {
        if (_securityLoaded)
        {
            return Task.CompletedTask;
        }
        _securityLoaded = true;
        return RefreshSecurityAsync();
    }

    public async Task RefreshSecurityAsync()
    {
        SecurityLoading = true;
        SecurityUnavailable = false;
        SecurityHasData = false;
        SecurityIsLimited = false;
        SecurityLimitedNote = string.Empty;

        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel
                .GetSecurityMetadataAsync(_pid, _createTime100ns).ConfigureAwait(false);

            Post(() =>
            {
                SecurityLoading = false;

                if (!outcome.Supported)
                {
                    SecurityUnavailable = true;
                    SecurityUnavailableText =
                        "Security details are unavailable — the connected service is too old.";
                    return;
                }

                var reply = outcome.Value;
                if (!reply.Available)
                {
                    SecurityUnavailable = true;
                    SecurityUnavailableText = R2Formatter.UnavailableReason(
                        reply.UnavailableReason,
                        "Security details couldn't be read for this process.");
                    return;
                }

                ApplySecurity(reply.Metadata);
                SecurityHasData = true;
            });
        }
        catch (Exception ex)
        {
            Post(() =>
            {
                SecurityLoading = false;
                SecurityUnavailable = true;
                SecurityUnavailableText = $"Could not reach the service: {ex.Message}";
            });
        }
    }

    private void ApplySecurity(SecurityMetadata m)
    {
        // Signature (reuse the R2 trust token — unsigned is caution, not danger).
        SecuritySignature = R2Formatter.SignatureStatusLabel(m.SignatureStatus);
        SecuritySignatureToken = R2Formatter.SignatureTrustToken(m.SignatureStatus);

        CertChain.Clear();
        long nowMs = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
        foreach (var c in m.CertChain)
        {
            CertChain.Add(new CertRowItem(
                SecurityFormatter.CertNameText(c.Subject),
                SecurityFormatter.CertNameText(c.Issuer),
                SecurityFormatter.ThumbprintGrouped(c.ThumbprintSha1),
                SecurityFormatter.CertValidUntil(c.NotAfterMs),
                SecurityFormatter.CertValidityNote(c.NotBeforeMs, c.NotAfterMs, nowMs),
                SecurityFormatter.CertValidityToken(c.NotBeforeMs, c.NotAfterMs, nowMs)));
        }
        SecurityCertChainEmpty = CertChain.Count == 0;

        // File hash.
        SecuritySha256Grouped = SecurityFormatter.Sha256Grouped(m.FileSha256);
        SecuritySha256Raw = SecurityFormatter.Sha256Raw(m.FileSha256);
        SecurityHasSha256 = SecuritySha256Raw.Length > 0;

        // Token identity.
        SecurityIntegrity = R2Formatter.IntegrityLabel(m.IntegrityLevel);
        SecurityElevation = R2Formatter.ElevationLabel(m.Elevated);
        SecurityAppContainer = SecurityFormatter.AppContainerLabel(m.AppContainer);
        SecurityUserSid = R2Formatter.OrDash(m.UserSid);

        Privileges.Clear();
        foreach (var p in m.Privileges)
        {
            Privileges.Add(new PrivilegeRowItem(
                SecurityFormatter.PrivilegeNameText(p.Name),
                SecurityFormatter.PrivilegeGloss(p.Name),
                SecurityFormatter.PrivilegeStateLabel(p.Enabled),
                SecurityFormatter.PrivilegeStateToken(p.Enabled)));
        }
        SecurityPrivilegesEmpty = Privileges.Count == 0;

        Groups.Clear();
        foreach (var g in m.Groups)
        {
            Groups.Add(SecurityFormatter.GroupText(g));
        }
        SecurityGroupsEmpty = Groups.Count == 0;

        Capabilities.Clear();
        foreach (var cap in m.Capabilities)
        {
            Capabilities.Add(SecurityFormatter.CapabilityText(cap));
        }
        SecurityCapabilitiesEmpty = Capabilities.Count == 0;

        // Mitigations (on-policies as chips).
        Mitigations.Clear();
        foreach (var mit in m.Mitigations)
        {
            if (!string.IsNullOrWhiteSpace(mit))
            {
                Mitigations.Add(SecurityFormatter.MitigationLabel(mit));
            }
        }
        SecurityMitigationsEmpty = Mitigations.Count == 0;

        // Honest coverage (alongside the partial data, never instead of it).
        SecurityIsLimited = m.Limited;
        SecurityLimitedNote = SecurityFormatter.LimitedCoverageNote(m.Limited);
    }

    // ----------------------------------------------------------------------

    private static string FormatEpochMs(long ms)
    {
        if (ms <= 0)
        {
            return "—";
        }
        try
        {
            return DateTimeOffset.FromUnixTimeMilliseconds(ms).LocalDateTime
                .ToString("yyyy-MM-dd HH:mm:ss", Inv);
        }
        catch
        {
            return "—";
        }
    }

    private void Post(Action action) => _dispatcher.TryEnqueue(() => action());
}

/// <summary>One handle row (type / name / handle / access), pre-formatted.</summary>
public sealed class HandleRowItem
{
    public string TypeText { get; }
    public string NameText { get; }
    public string HandleText { get; }
    public string AccessText { get; }

    public HandleRowItem(string typeText, string nameText, string handleText, string accessText)
    {
        TypeText = typeText;
        NameText = nameText;
        HandleText = handleText;
        AccessText = accessText;
    }
}

/// <summary>One module row (name / path / version / publisher / signed), pre-formatted.</summary>
public sealed class ModuleRowItem
{
    public string NameText { get; }
    public string PathText { get; }
    public string VersionText { get; }
    public string PublisherText { get; }
    public string SignedText { get; }

    /// <summary>Trust token ("signed"/"caution") for the signed pill color.</summary>
    public string SignedToken { get; }

    public ModuleRowItem(
        string nameText, string pathText, string versionText,
        string publisherText, string signedText, string signedToken)
    {
        NameText = nameText;
        PathText = pathText;
        VersionText = versionText;
        PublisherText = publisherText;
        SignedText = signedText;
        SignedToken = signedToken;
    }
}

/// <summary>
/// One signing-certificate row (leaf → root), pre-formatted. A validity note
/// (expired / expiring soon / not yet valid) is stated as a calm fact; its token
/// tops out at caution, never danger.
/// </summary>
public sealed class CertRowItem
{
    public string SubjectText { get; }
    public string IssuerText { get; }
    public string ThumbprintText { get; }
    public string ValidUntilText { get; }
    public string ValidityNote { get; }

    /// <summary>Trust token ("ok"/"caution"/"expired") for the validity note color.</summary>
    public string ValidityToken { get; }

    /// <summary>True when there is a validity note to show (drives its visibility).</summary>
    public bool HasValidityNote { get; }

    public CertRowItem(
        string subjectText, string issuerText, string thumbprintText,
        string validUntilText, string validityNote, string validityToken)
    {
        SubjectText = subjectText;
        IssuerText = issuerText;
        ThumbprintText = thumbprintText;
        ValidUntilText = validUntilText;
        ValidityNote = validityNote;
        ValidityToken = validityToken;
        HasValidityNote = validityNote.Length > 0;
    }
}

/// <summary>
/// One token-privilege row (SeXxx name + friendly gloss + enabled/available
/// state), pre-formatted. The state is neutral-informational — a held privilege
/// is normal, so it is never colored as an alarm.
/// </summary>
public sealed class PrivilegeRowItem
{
    public string NameText { get; }
    public string GlossText { get; }
    public string StateText { get; }

    /// <summary>State token ("enabled"/"available") for the neutral pill color.</summary>
    public string StateToken { get; }

    /// <summary>True when a friendly gloss is available (drives its visibility).</summary>
    public bool HasGloss { get; }

    public PrivilegeRowItem(string nameText, string glossText, string stateText, string stateToken)
    {
        NameText = nameText;
        GlossText = glossText;
        StateText = stateText;
        StateToken = stateToken;
        HasGloss = glossText.Length > 0;
    }
}

/// <summary>One thread row, pre-formatted for the table.</summary>
public sealed class ThreadRowItem
{
    public string TidText { get; }
    public string StartAddressText { get; }
    public string StateText { get; }
    public string WaitReasonText { get; }
    public string PriorityText { get; }
    public string CpuText { get; }
    public string CpuTimeText { get; }

    public ThreadRowItem(
        string tidText, string startAddressText, string stateText, string waitReasonText,
        string priorityText, string cpuText, string cpuTimeText)
    {
        TidText = tidText;
        StartAddressText = startAddressText;
        StateText = stateText;
        WaitReasonText = waitReasonText;
        PriorityText = priorityText;
        CpuText = cpuText;
        CpuTimeText = cpuTimeText;
    }
}
