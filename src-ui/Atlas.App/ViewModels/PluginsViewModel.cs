using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using Atlas.IpcClient;
using Atlas.V0;
using CommunityToolkit.Mvvm.ComponentModel;
using Microsoft.UI.Dispatching;

namespace Atlas.App.ViewModels;

/// <summary>
/// Drives the Plugins page (R3, PRD §18.3): the registry of signed, out-of-process,
/// capability-scoped <b>read-only</b> extensions. The list shows each plugin's name,
/// version, publisher, signature badge, granted read-only capabilities (as chips),
/// an enabled toggle, and remove; the page orchestrates the register and grant-edit
/// dialogs and hands the results back to this view-model, which owns every call.
///
/// <para>
/// The security framing is the point and must be unmistakable but never
/// fear-mongering: plugins run in their own process, can only ever <em>read</em> a
/// slice of data you grant, and are off until you enable them. Unsigned is a
/// caution, not a threat.
/// </para>
///
/// <para>
/// AtlasPlugins is a NEW service that lands server-side after this UI, so every call
/// degrades gracefully: an <c>Unimplemented</c> reply becomes a calm "unavailable —
/// the service is too old" placeholder rather than a crash (task brief). Set
/// <c>ATLAS_FAKE_PLUGINS=1</c> to populate the page with sample data for previewing
/// the UX without a backend.
/// </para>
/// </summary>
public sealed partial class PluginsViewModel : ObservableObject
{
    private readonly DispatcherQueue _dispatcher;
    private readonly string? _who;
    private readonly bool _fake;
    private CancellationTokenSource? _cts;

    // The in-memory registry used in demo mode (no backend). Mutated by the fake
    // paths of register/grant/enable/remove so the whole UX is coherent offline.
    private readonly List<Plugin> _fakeStore = new();

    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private bool _isUnavailable;
    [ObservableProperty] private bool _hasLoaded;
    [ObservableProperty] private bool _isEmpty;
    [ObservableProperty] private string _statusText = string.Empty;

    public ObservableCollection<PluginRowViewModel> Plugins { get; } = new();

    public PluginsViewModel(DispatcherQueue dispatcher, string? who = null, bool fake = false)
    {
        _dispatcher = dispatcher;
        _who = who;
        _fake = fake;
    }

    /// <summary>Whether this page is running against demo data (no backend).</summary>
    public bool IsFake => _fake;

    /// <summary>Loads (or reloads) the plugin registry.</summary>
    public async Task RefreshAsync()
    {
        _cts?.Cancel();
        var cts = new CancellationTokenSource();
        _cts = cts;
        var ct = cts.Token;

        IsLoading = true;
        IsUnavailable = false;
        IsEmpty = false;
        StatusText = "Loading…";

        if (_fake)
        {
            LoadFake();
            return;
        }

        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.ListPluginsAsync(ct).ConfigureAwait(false);
            if (ct.IsCancellationRequested)
            {
                return;
            }

            if (!outcome.Supported)
            {
                Post(() =>
                {
                    Plugins.Clear();
                    IsUnavailable = true;
                    HasLoaded = true;
                    StatusText = "Plugins unavailable — the connected service is too old.";
                    IsLoading = false;
                });
                return;
            }

            Post(() =>
            {
                Plugins.Clear();
                foreach (var p in outcome.Value.Plugins)
                {
                    Plugins.Add(new PluginRowViewModel(p));
                }
                IsEmpty = Plugins.Count == 0;
                HasLoaded = true;
                StatusText = Plugins.Count == 0
                    ? "No plugins registered."
                    : $"{Plugins.Count} plugin{(Plugins.Count == 1 ? "" : "s")} registered.";
                IsLoading = false;
            });
        }
        catch (OperationCanceledException)
        {
            // Superseded by a newer refresh.
        }
        catch (Exception ex)
        {
            Post(() =>
            {
                Plugins.Clear();
                IsUnavailable = true;
                HasLoaded = true;
                StatusText = $"Could not reach the service: {ex.Message}";
                IsLoading = false;
            });
        }
    }

    /// <summary>
    /// Registers a plugin executable with an initial granted capability set.
    /// Returns the server-side result: an unsigned executable is refused unless
    /// <paramref name="allowUnsigned"/> is set (the refusal reason is surfaced), and
    /// a newly registered plugin is disabled until the user enables it. Refreshes on
    /// success so the new row appears.
    /// </summary>
    public async Task<(bool ok, string message)> RegisterPluginAsync(
        string exePath, IReadOnlyList<PluginCapability> capabilities, bool allowUnsigned)
    {
        if (_fake)
        {
            var (ok, message) = FakeRegister(exePath, capabilities, allowUnsigned);
            if (ok)
            {
                LoadFake();
            }
            return (ok, message);
        }

        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.RegisterPluginAsync(exePath, capabilities, allowUnsigned)
                .ConfigureAwait(false);
            if (!outcome.Supported)
            {
                return (false, "This service is too old to manage plugins.");
            }
            if (!outcome.Value.Ok)
            {
                return (false, string.IsNullOrEmpty(outcome.Value.Message)
                    ? "The service could not register this plugin."
                    : outcome.Value.Message);
            }
            await RefreshAsync().ConfigureAwait(false);
            return (true, string.IsNullOrEmpty(outcome.Value.Message)
                ? "Plugin registered."
                : outcome.Value.Message);
        }
        catch (Exception ex)
        {
            return (false, ex.Message);
        }
    }

    /// <summary>
    /// Replaces the granted read-only capabilities for a plugin (a re-grant). A
    /// plugin only ever gets what the user grants here. Reflects the new grant on the
    /// row on success.
    /// </summary>
    public async Task<(bool ok, string message)> GrantCapabilitiesAsync(
        long pluginId, IReadOnlyList<PluginCapability> granted)
    {
        if (_fake)
        {
            FakeGrant(pluginId, granted);
            Post(() => FindRow(pluginId)?.ApplyGranted(granted));
            return (true, string.Empty);
        }

        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.GrantPluginCapabilitiesAsync(pluginId, granted)
                .ConfigureAwait(false);
            if (!outcome.Supported)
            {
                return (false, "This service is too old to manage plugin capabilities.");
            }
            if (!outcome.Value.Ok)
            {
                return (false, "The service could not update this plugin's capabilities.");
            }
            Post(() => FindRow(pluginId)?.ApplyGranted(granted));
            return (true, string.Empty);
        }
        catch (Exception ex)
        {
            return (false, ex.Message);
        }
    }

    /// <summary>
    /// Enables or disables a plugin. Enabling IS the consent gesture that lets it be
    /// launched with its granted read-only capabilities; disabling stops it.
    /// </summary>
    public async Task<(bool ok, string message)> SetEnabledAsync(long pluginId, bool enabled)
    {
        if (_fake)
        {
            FakeSetEnabled(pluginId, enabled);
            Post(() => FindRow(pluginId)?.ApplyEnabled(enabled));
            return (true, string.Empty);
        }

        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.SetPluginEnabledAsync(pluginId, enabled).ConfigureAwait(false);
            if (!outcome.Supported)
            {
                return (false, "This service is too old to enable or disable plugins.");
            }
            if (!outcome.Value.Ok)
            {
                return (false, string.IsNullOrEmpty(outcome.Value.Message)
                    ? "The service could not change this plugin."
                    : outcome.Value.Message);
            }
            Post(() => FindRow(pluginId)?.ApplyEnabled(enabled));
            return (true, string.Empty);
        }
        catch (Exception ex)
        {
            return (false, ex.Message);
        }
    }

    /// <summary>Removes a plugin from the registry by id.</summary>
    public async Task<(bool ok, string message)> RemovePluginAsync(long pluginId)
    {
        if (_fake)
        {
            FakeRemove(pluginId);
            LoadFake();
            return (true, string.Empty);
        }

        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.RemovePluginAsync(pluginId).ConfigureAwait(false);
            if (!outcome.Supported)
            {
                return (false, "This service is too old to remove plugins.");
            }
            return outcome.Value.Ok
                ? (true, string.Empty)
                : (false, "The service could not remove this plugin.");
        }
        catch (Exception ex)
        {
            return (false, ex.Message);
        }
    }

    // ----------------------------------------------------------------------
    // Demo mode (ATLAS_FAKE_PLUGINS=1). A tiny in-memory registry so the whole
    // register/grant/enable/remove UX — and its security framing — can be seen
    // without a backend.
    // ----------------------------------------------------------------------

    private void LoadFake()
    {
        if (_fakeStore.Count == 0 && !HasLoaded)
        {
            _fakeStore.AddRange(PluginsDemoData.SamplePlugins());
        }
        Post(() =>
        {
            Plugins.Clear();
            foreach (var p in _fakeStore)
            {
                Plugins.Add(new PluginRowViewModel(p));
            }
            IsUnavailable = false;
            IsEmpty = Plugins.Count == 0;
            HasLoaded = true;
            StatusText = Plugins.Count == 0
                ? "No plugins registered (demo data)."
                : $"{Plugins.Count} plugin{(Plugins.Count == 1 ? "" : "s")} registered (demo data).";
            IsLoading = false;
        });
    }

    private (bool ok, string message) FakeRegister(
        string exePath, IReadOnlyList<PluginCapability> capabilities, bool allowUnsigned)
    {
        // Mirror the server's core rule in demo mode: an unsigned executable is
        // refused unless the user explicitly opted in. Treat a path containing
        // "unsigned" as an unsigned binary so the refusal copy can be previewed.
        bool looksUnsigned = exePath.Contains("unsigned", StringComparison.OrdinalIgnoreCase);
        if (looksUnsigned && !allowUnsigned)
        {
            return (false, "refused: executable is not signed. Enable “allow unsigned” only if you trust the source.");
        }

        var name = System.IO.Path.GetFileNameWithoutExtension(exePath);
        var plugin = new Plugin
        {
            Id = (_fakeStore.Count == 0 ? 0 : _fakeStore.Max(p => p.Id)) + 1,
            Name = string.IsNullOrWhiteSpace(name) ? "New plugin" : name,
            Version = "1.0.0",
            Publisher = looksUnsigned ? string.Empty : "Contoso Ltd.",
            ExePath = exePath,
            Signature = looksUnsigned ? PluginSignature.PluginUnsigned : PluginSignature.PluginSigned,
            Enabled = false,
            RegisteredMs = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds(),
            Description = "Registered in demo mode.",
        };
        plugin.Granted.AddRange(PluginFormatter.NormalizeCapabilities(capabilities));
        _fakeStore.Add(plugin);
        return (true, looksUnsigned
            ? "Unsigned plugin registered (demo). It stays off until you enable it."
            : "Plugin registered (demo). It stays off until you enable it.");
    }

    private void FakeGrant(long pluginId, IReadOnlyList<PluginCapability> granted)
    {
        var p = _fakeStore.FirstOrDefault(x => x.Id == pluginId);
        if (p is null)
        {
            return;
        }
        p.Granted.Clear();
        p.Granted.AddRange(PluginFormatter.NormalizeCapabilities(granted));
    }

    private void FakeSetEnabled(long pluginId, bool enabled)
    {
        var p = _fakeStore.FirstOrDefault(x => x.Id == pluginId);
        if (p is not null)
        {
            p.Enabled = enabled;
        }
    }

    private void FakeRemove(long pluginId)
    {
        var p = _fakeStore.FirstOrDefault(x => x.Id == pluginId);
        if (p is not null)
        {
            _fakeStore.Remove(p);
        }
    }

    private PluginRowViewModel? FindRow(long pluginId)
    {
        foreach (var r in Plugins)
        {
            if (r.Id == pluginId)
            {
                return r;
            }
        }
        return null;
    }

    public void Stop() => _cts?.Cancel();

    private void Post(Action action) => _dispatcher.TryEnqueue(() => action());
}

/// <summary>
/// One plugin row: the raw <see cref="Plugin"/> (source of truth for the grant
/// dialog) plus pre-formatted display. Observable so the enabled toggle and a
/// re-grant reflect live without a full reload.
/// </summary>
public sealed partial class PluginRowViewModel : ObservableObject
{
    [ObservableProperty] private bool _enabled;

    public PluginRowViewModel(Plugin plugin)
    {
        Plugin = plugin;
        _enabled = plugin.Enabled;
        RebuildChips();
    }

    /// <summary>The underlying plugin (source of truth for grant editing).</summary>
    public Plugin Plugin { get; private set; }

    public long Id => Plugin.Id;
    public string Name => PluginFormatter.PluginName(Plugin);
    public string VersionText => PluginFormatter.VersionText(Plugin.Version);
    public bool HasVersion => VersionText.Length > 0;
    public string PublisherText => PluginFormatter.PublisherText(Plugin.Publisher);
    public string ExePath => string.IsNullOrWhiteSpace(Plugin.ExePath) ? "—" : Plugin.ExePath;

    public string SignatureLabel => PluginFormatter.SignatureLabel(Plugin.Signature);
    public string SignatureToken => PluginFormatter.SignatureColorToken(Plugin.Signature);
    public string SignatureGlyph => PluginFormatter.SignatureGlyph(Plugin.Signature);
    public string SignatureNote => PluginFormatter.SignatureNote(Plugin.Signature);

    /// <summary>True for an unsigned plugin, so the row can show the calm caution note.</summary>
    public bool IsUnsigned => Plugin.Signature == PluginSignature.PluginUnsigned;

    public string GrantedCountText => PluginFormatter.GrantedCountText(Plugin.Granted);
    public string GrantedSummary => PluginFormatter.GrantedSummary(Plugin.Granted);
    public bool HasNoCapabilities => PluginFormatter.NormalizeCapabilities(Plugin.Granted).Count == 0;

    /// <summary>The capability chips to render on the row.</summary>
    public ObservableCollection<CapabilityChip> Chips { get; } = new();

    /// <summary>The currently granted capabilities, canonical order — seeds the grant dialog.</summary>
    public IReadOnlyList<PluginCapability> GrantedCapabilities =>
        PluginFormatter.NormalizeCapabilities(Plugin.Granted);

    /// <summary>Reflects a confirmed enabled-state change from the service.</summary>
    public void ApplyEnabled(bool enabled)
    {
        Plugin.Enabled = enabled;
        Enabled = enabled;
    }

    /// <summary>Reflects a confirmed re-grant from the service and refreshes display.</summary>
    public void ApplyGranted(IReadOnlyList<PluginCapability> granted)
    {
        Plugin.Granted.Clear();
        Plugin.Granted.AddRange(PluginFormatter.NormalizeCapabilities(granted));
        RebuildChips();
        OnPropertyChanged(nameof(GrantedCountText));
        OnPropertyChanged(nameof(GrantedSummary));
        OnPropertyChanged(nameof(HasNoCapabilities));
        OnPropertyChanged(nameof(GrantedCapabilities));
    }

    private void RebuildChips()
    {
        Chips.Clear();
        foreach (var cap in PluginFormatter.NormalizeCapabilities(Plugin.Granted))
        {
            Chips.Add(new CapabilityChip(
                PluginFormatter.CapabilityLabel(cap),
                PluginFormatter.CapabilityGlyph(cap)));
        }
    }
}

/// <summary>One granted-capability chip (pre-formatted label + glyph).</summary>
public sealed class CapabilityChip
{
    public CapabilityChip(string label, string glyph)
    {
        Label = label;
        Glyph = glyph;
    }

    public string Label { get; }
    public string Glyph { get; }
}

/// <summary>
/// One selectable capability in the register/grant dialogs: the read-only slice,
/// its friendly label + description, and whether the user has ticked it. The whole
/// framing hinges on this — the user sees exactly which read-only slice each toggle
/// grants, and a plugin gets ONLY what is ticked here.
/// </summary>
public sealed partial class CapabilityChoiceViewModel : ObservableObject
{
    [ObservableProperty] private bool _isSelected;

    public CapabilityChoiceViewModel(PluginCapability capability, bool selected)
    {
        Capability = capability;
        _isSelected = selected;
        Label = PluginFormatter.CapabilityLabel(capability);
        Description = PluginFormatter.CapabilityDescription(capability);
        Glyph = PluginFormatter.CapabilityGlyph(capability);
    }

    public PluginCapability Capability { get; }
    public string Label { get; }
    public string Description { get; }
    public string Glyph { get; }
}
