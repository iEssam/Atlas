using System;
using System.Collections.ObjectModel;
using System.Threading;
using System.Threading.Tasks;
using Atlas.IpcClient;
using Atlas.V0;
using CommunityToolkit.Mvvm.ComponentModel;
using Microsoft.UI.Dispatching;

namespace Atlas.App.ViewModels;

/// <summary>
/// Drives the Services page (M7, PRD §9.9.1): a virtualized, filterable table of
/// Windows services — display name, service name, state (colored), start type,
/// PID, account — with a detail pane showing the binary path and description for
/// the selected row. The filter box debounces so we don't hammer the service on
/// every keystroke. Read-only this milestone. Degrades gracefully when the
/// service is too old (Unimplemented → inline "unavailable" placeholder).
/// </summary>
public sealed partial class ServicesViewModel : ObservableObject
{
    /// <summary>Debounce window for filter typing before a query fires.</summary>
    private static readonly TimeSpan FilterDebounce = TimeSpan.FromMilliseconds(350);

    private readonly DispatcherQueue _dispatcher;
    private readonly string? _who;
    private CancellationTokenSource? _cts;
    private CancellationTokenSource? _debounceCts;

    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private bool _isUnavailable;
    [ObservableProperty] private bool _hasLoaded;
    [ObservableProperty] private bool _isEmpty;
    [ObservableProperty] private string _statusText = string.Empty;

    [ObservableProperty] private string _filter = string.Empty;

    [ObservableProperty] private ServiceItem? _selectedService;

    /// <summary>True when a row is selected, so the detail pane can show/hide.</summary>
    public bool HasSelection => SelectedService is not null;

    public ObservableCollection<ServiceItem> Services { get; } = new();

    public ServicesViewModel(DispatcherQueue dispatcher, string? who = null)
    {
        _dispatcher = dispatcher;
        _who = who;
    }

    partial void OnSelectedServiceChanged(ServiceItem? value) => OnPropertyChanged(nameof(HasSelection));

    /// <summary>
    /// Called on each filter edit; debounces before querying so rapid typing
    /// issues a single request. Awaiting the delay under a fresh token means a
    /// newer keystroke cancels the pending query.
    /// </summary>
    partial void OnFilterChanged(string value)
    {
        _debounceCts?.Cancel();
        var cts = new CancellationTokenSource();
        _debounceCts = cts;
        _ = DebouncedRefreshAsync(cts.Token);
    }

    private async Task DebouncedRefreshAsync(CancellationToken ct)
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
            await RefreshAsync().ConfigureAwait(false);
        }
    }

    /// <summary>Loads (or reloads) the service list for the current filter.</summary>
    public async Task RefreshAsync()
    {
        _cts?.Cancel();
        var cts = new CancellationTokenSource();
        _cts = cts;
        var ct = cts.Token;

        var filter = Filter?.Trim() ?? string.Empty;

        IsLoading = true;
        IsUnavailable = false;
        IsEmpty = false;
        StatusText = "Loading…";

        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.ListServicesAsync(filter, ct).ConfigureAwait(false);

            if (ct.IsCancellationRequested)
            {
                return;
            }

            if (!outcome.Supported)
            {
                Post(() =>
                {
                    Services.Clear();
                    SelectedService = null;
                    IsUnavailable = true;
                    HasLoaded = true;
                    StatusText = "Services unavailable — the service is too old.";
                    IsLoading = false;
                });
                return;
            }

            Post(() =>
            {
                Services.Clear();
                foreach (var s in outcome.Value.Services)
                {
                    Services.Add(new ServiceItem(
                        string.IsNullOrEmpty(s.DisplayName) ? s.Name : s.DisplayName,
                        s.Name,
                        M7Formatter.ServiceStateLabel(s.State),
                        M7Formatter.ServiceStateSeverity(s.State),
                        M7Formatter.ServiceStartTypeLabel(s.StartType, s.DelayedAutoStart),
                        M7Formatter.PidText(s.Pid),
                        string.IsNullOrEmpty(s.Account) ? "—" : s.Account,
                        string.IsNullOrEmpty(s.BinaryPath) ? "—" : s.BinaryPath,
                        string.IsNullOrEmpty(s.Description) ? "No description." : s.Description));
                }
                SelectedService = null;

                IsEmpty = Services.Count == 0;
                HasLoaded = true;
                StatusText = Services.Count == 0
                    ? (filter.Length == 0 ? "No services found." : $"No services match “{filter}”.")
                    : $"{Services.Count} service{(Services.Count == 1 ? "" : "s")}"
                        + (filter.Length == 0 ? "." : $" matching “{filter}”.");
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
                Services.Clear();
                SelectedService = null;
                IsUnavailable = true;
                HasLoaded = true;
                StatusText = $"Could not reach the service: {ex.Message}";
                IsLoading = false;
            });
        }
    }

    public void Stop()
    {
        _debounceCts?.Cancel();
        _cts?.Cancel();
    }

    private void Post(Action action) => _dispatcher.TryEnqueue(() => action());
}

/// <summary>One service row, pre-formatted for the table + detail pane.</summary>
public sealed class ServiceItem
{
    public string DisplayName { get; }
    public string ServiceName { get; }
    public string StateText { get; }

    /// <summary>Severity token ("running"/"transitional"/"stopped"/"unknown").</summary>
    public string StateSeverity { get; }
    public string StartTypeText { get; }
    public string PidText { get; }
    public string Account { get; }
    public string BinaryPath { get; }
    public string Description { get; }

    public bool IsRunning => StateSeverity == "running";
    public bool IsStopped => StateSeverity == "stopped";
    public bool IsTransitional => StateSeverity == "transitional";

    public ServiceItem(
        string displayName, string serviceName, string stateText, string stateSeverity,
        string startTypeText, string pidText, string account, string binaryPath, string description)
    {
        DisplayName = displayName;
        ServiceName = serviceName;
        StateText = stateText;
        StateSeverity = stateSeverity;
        StartTypeText = startTypeText;
        PidText = pidText;
        Account = account;
        BinaryPath = binaryPath;
        Description = description;
    }
}
