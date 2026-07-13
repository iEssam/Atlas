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
/// Drives the Network page (R2, PRD §9.12): a virtualized, filterable table of the
/// system's network connections — owning app/PID, protocol, local and remote
/// endpoints, resolved domain, and a colored TCP state — plus a Listening-ports
/// sub-view that lists the bound TCP/UDP ports and their owners. Modelled directly
/// on the M7 Services page (debounced filter, empty/unavailable placeholders,
/// read-only), applied to connections.
///
/// <para>
/// The proto's <c>ListConnections</c> takes no text filter, so the filter is
/// applied <b>client-side</b> over the fetched rows (debounced so rapid typing
/// re-filters once). Switching sub-view re-queries the service. Degrades gracefully
/// when the service is too old (Unimplemented → inline "unavailable" placeholder).
/// </para>
/// </summary>
public sealed partial class NetworkViewModel : ObservableObject
{
    /// <summary>Debounce window for filter typing before the list re-filters.</summary>
    private static readonly TimeSpan FilterDebounce = TimeSpan.FromMilliseconds(300);

    private readonly DispatcherQueue _dispatcher;
    private readonly string? _who;
    private CancellationTokenSource? _cts;
    private CancellationTokenSource? _debounceCts;

    // The full, unfiltered fetch for the current sub-view; the visible collections
    // are these re-projected through the current filter.
    private readonly List<ConnectionItem> _allConnections = new();
    private readonly List<ListeningPortItem> _allPorts = new();

    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private bool _isUnavailable;
    [ObservableProperty] private bool _hasLoaded;
    [ObservableProperty] private bool _isEmpty;
    [ObservableProperty] private string _statusText = string.Empty;
    [ObservableProperty] private string _unavailableText = string.Empty;

    [ObservableProperty] private string _filter = string.Empty;

    /// <summary>
    /// When true the page shows the Listening-ports sub-view; otherwise the active
    /// connections table. Toggling re-queries the service.
    /// </summary>
    [ObservableProperty] private bool _showListening;

    /// <summary>True in the connections sub-view (inverse of <see cref="ShowListening"/>).</summary>
    public bool ShowConnections => !ShowListening;

    public ObservableCollection<ConnectionItem> Connections { get; } = new();
    public ObservableCollection<ListeningPortItem> Ports { get; } = new();

    public NetworkViewModel(DispatcherQueue dispatcher, string? who = null)
    {
        _dispatcher = dispatcher;
        _who = who;
    }

    partial void OnShowListeningChanged(bool value)
    {
        OnPropertyChanged(nameof(ShowConnections));
        _ = RefreshAsync();
    }

    /// <summary>Debounces filter edits, then re-applies the filter client-side.</summary>
    partial void OnFilterChanged(string value)
    {
        _debounceCts?.Cancel();
        var cts = new CancellationTokenSource();
        _debounceCts = cts;
        _ = DebouncedFilterAsync(cts.Token);
    }

    private async Task DebouncedFilterAsync(CancellationToken ct)
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
            _dispatcher.TryEnqueue(ApplyFilter);
        }
    }

    /// <summary>Fetches the current sub-view from the service.</summary>
    public async Task RefreshAsync()
    {
        _cts?.Cancel();
        var cts = new CancellationTokenSource();
        _cts = cts;
        var ct = cts.Token;

        var listening = ShowListening;

        IsLoading = true;
        IsUnavailable = false;
        IsEmpty = false;
        StatusText = "Loading…";

        try
        {
            using var channel = AtlasChannel.Connect(_who);

            if (listening)
            {
                var outcome = await channel.ListListeningPortsAsync(ct).ConfigureAwait(false);
                if (ct.IsCancellationRequested)
                {
                    return;
                }
                if (!outcome.Supported)
                {
                    PostUnsupported();
                    return;
                }
                var items = outcome.Value.Ports
                    .Select(p => new ListeningPortItem(
                        MonitorFormatter.ProcessText(p.ImageName, p.Pid),
                        p.Pid,
                        MonitorFormatter.L4ProtocolLabel(p.Protocol),
                        MonitorFormatter.EndpointText(p.BindAddr, p.Port, p.IsIpv6),
                        p.IsIpv6 ? "IPv6" : "IPv4"))
                    .ToList();
                Post(() =>
                {
                    _allPorts.Clear();
                    _allPorts.AddRange(items);
                    ApplyFilter();
                    HasLoaded = true;
                    IsLoading = false;
                });
            }
            else
            {
                // Fetch active connections only; the listening endpoints live in
                // their own sub-view.
                var outcome = await channel.ListConnectionsAsync(includeListening: false, ct)
                    .ConfigureAwait(false);
                if (ct.IsCancellationRequested)
                {
                    return;
                }
                if (!outcome.Supported)
                {
                    PostUnsupported();
                    return;
                }
                var items = outcome.Value.Connections
                    .Select(c => new ConnectionItem(
                        MonitorFormatter.ProcessText(c.ImageName, c.Pid),
                        c.Pid,
                        MonitorFormatter.L4ProtocolLabel(c.Protocol),
                        MonitorFormatter.EndpointText(c.LocalAddr, c.LocalPort, c.IsIpv6),
                        MonitorFormatter.EndpointText(c.RemoteAddr, c.RemotePort, c.IsIpv6),
                        MonitorFormatter.DomainText(c.RemoteDomain),
                        MonitorFormatter.TcpStateLabel(c.State),
                        MonitorFormatter.TcpStateToken(c.State)))
                    .ToList();
                Post(() =>
                {
                    _allConnections.Clear();
                    _allConnections.AddRange(items);
                    ApplyFilter();
                    HasLoaded = true;
                    IsLoading = false;
                });
            }
        }
        catch (OperationCanceledException)
        {
            // Superseded by a newer refresh.
        }
        catch (Exception ex)
        {
            Post(() =>
            {
                _allConnections.Clear();
                _allPorts.Clear();
                Connections.Clear();
                Ports.Clear();
                IsUnavailable = true;
                UnavailableText = $"Could not reach the service: {ex.Message}";
                HasLoaded = true;
                StatusText = string.Empty;
                IsLoading = false;
            });
        }
    }

    /// <summary>Re-projects the fetched rows through the current filter (client-side).</summary>
    private void ApplyFilter()
    {
        var filter = (Filter ?? string.Empty).Trim();
        var hasFilter = filter.Length > 0;

        if (ShowListening)
        {
            Ports.Clear();
            IEnumerable<ListeningPortItem> src = _allPorts;
            if (hasFilter)
            {
                src = src.Where(p => p.MatchesFilter(filter));
            }
            foreach (var p in src)
            {
                Ports.Add(p);
            }
            IsEmpty = Ports.Count == 0;
            StatusText = BuildStatus(Ports.Count, _allPorts.Count, hasFilter, filter, "listening port");
        }
        else
        {
            Connections.Clear();
            IEnumerable<ConnectionItem> src = _allConnections;
            if (hasFilter)
            {
                src = src.Where(c => c.MatchesFilter(filter));
            }
            foreach (var c in src)
            {
                Connections.Add(c);
            }
            IsEmpty = Connections.Count == 0;
            StatusText = BuildStatus(Connections.Count, _allConnections.Count, hasFilter, filter, "connection");
        }
    }

    private static string BuildStatus(int shown, int total, bool hasFilter, string filter, string noun)
    {
        if (total == 0)
        {
            return $"No {noun}s found.";
        }
        if (shown == 0)
        {
            return $"No {noun}s match “{filter}”.";
        }
        var plural = shown == 1 ? "" : "s";
        return hasFilter
            ? $"{shown} of {total} {noun}{plural} matching “{filter}”."
            : $"{shown} {noun}{plural}.";
    }

    private void PostUnsupported() => Post(() =>
    {
        _allConnections.Clear();
        _allPorts.Clear();
        Connections.Clear();
        Ports.Clear();
        IsUnavailable = true;
        UnavailableText =
            "Network monitoring needs a newer Atlas — the connected service is too old to list connections.";
        HasLoaded = true;
        StatusText = string.Empty;
        IsLoading = false;
    });

    public void Stop()
    {
        _debounceCts?.Cancel();
        _cts?.Cancel();
    }

    private void Post(Action action) => _dispatcher.TryEnqueue(() => action());
}

/// <summary>One network connection row, pre-formatted for the table.</summary>
public sealed class ConnectionItem
{
    public string Process { get; }
    public uint Pid { get; }
    public string Protocol { get; }
    public string LocalEndpoint { get; }
    public string RemoteEndpoint { get; }
    public string Domain { get; }
    public string StateText { get; }

    /// <summary>Calm color token for the TCP state dot (never a danger token).</summary>
    public string StateToken { get; }

    public ConnectionItem(
        string process, uint pid, string protocol, string localEndpoint,
        string remoteEndpoint, string domain, string stateText, string stateToken)
    {
        Process = process;
        Pid = pid;
        Protocol = protocol;
        LocalEndpoint = localEndpoint;
        RemoteEndpoint = remoteEndpoint;
        Domain = domain;
        StateText = stateText;
        StateToken = stateToken;
    }

    public bool MatchesFilter(string filter) =>
        Process.Contains(filter, StringComparison.OrdinalIgnoreCase)
        || Protocol.Contains(filter, StringComparison.OrdinalIgnoreCase)
        || LocalEndpoint.Contains(filter, StringComparison.OrdinalIgnoreCase)
        || RemoteEndpoint.Contains(filter, StringComparison.OrdinalIgnoreCase)
        || Domain.Contains(filter, StringComparison.OrdinalIgnoreCase)
        || StateText.Contains(filter, StringComparison.OrdinalIgnoreCase);
}

/// <summary>One listening/bound port row, pre-formatted for the sub-view table.</summary>
public sealed class ListeningPortItem
{
    public string Process { get; }
    public uint Pid { get; }
    public string Protocol { get; }
    public string Endpoint { get; }
    public string Family { get; }

    public ListeningPortItem(string process, uint pid, string protocol, string endpoint, string family)
    {
        Process = process;
        Pid = pid;
        Protocol = protocol;
        Endpoint = endpoint;
        Family = family;
    }

    public bool MatchesFilter(string filter) =>
        Process.Contains(filter, StringComparison.OrdinalIgnoreCase)
        || Protocol.Contains(filter, StringComparison.OrdinalIgnoreCase)
        || Endpoint.Contains(filter, StringComparison.OrdinalIgnoreCase)
        || Family.Contains(filter, StringComparison.OrdinalIgnoreCase);
}
