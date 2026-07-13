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
/// Drives the Profiles page (R2, PRD §9.7.4): named, activatable bundles of rules
/// plus a power mode. Lists profiles (name, power mode, member rules, active
/// toggle), supports create/edit (pick member rules + power mode), and
/// activate/deactivate. Activating a profile enables its member rules and applies
/// its power mode; the page confirms first, listing exactly which rules it will
/// turn on (transparency — the user sees what changes).
///
/// <para>
/// Loads the rule list alongside the profiles so member rule ids resolve to names.
/// Degrades gracefully when AtlasRules is unavailable (it lands after this UI). Set
/// <c>ATLAS_FAKE_RULES=1</c> for demo data.
/// </para>
/// </summary>
public sealed partial class ProfilesViewModel : ObservableObject
{
    private readonly DispatcherQueue _dispatcher;
    private readonly string? _who;
    private readonly bool _fake;
    private CancellationTokenSource? _cts;

    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private bool _isUnavailable;
    [ObservableProperty] private bool _hasLoaded;
    [ObservableProperty] private bool _isEmpty;
    [ObservableProperty] private string _statusText = string.Empty;

    public ObservableCollection<ProfileRowViewModel> Profiles { get; } = new();

    /// <summary>All rules, for the member-picker and id→name resolution.</summary>
    public IReadOnlyList<Rule> AllRules { get; private set; } = Array.Empty<Rule>();

    public ProfilesViewModel(DispatcherQueue dispatcher, string? who = null, bool fake = false)
    {
        _dispatcher = dispatcher;
        _who = who;
        _fake = fake;
    }

    public bool IsFake => _fake;

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

            var profilesOutcome = await channel.ListProfilesAsync(ct).ConfigureAwait(false);
            if (ct.IsCancellationRequested)
            {
                return;
            }

            if (!profilesOutcome.Supported)
            {
                Post(() =>
                {
                    Profiles.Clear();
                    IsUnavailable = true;
                    HasLoaded = true;
                    StatusText = "Profiles unavailable — the connected service is too old.";
                    IsLoading = false;
                });
                return;
            }

            // Rules are best-effort context; a missing rule list just means member
            // names fall back to their ids.
            var rulesOutcome = await channel.ListRulesAsync(ct).ConfigureAwait(false);
            var rules = rulesOutcome.Supported
                ? (IReadOnlyList<Rule>)rulesOutcome.Value.Rules.ToList()
                : Array.Empty<Rule>();

            Post(() => Populate(profilesOutcome.Value.Profiles, rules, demo: false));
        }
        catch (OperationCanceledException)
        {
            // Superseded.
        }
        catch (Exception ex)
        {
            Post(() =>
            {
                Profiles.Clear();
                IsUnavailable = true;
                HasLoaded = true;
                StatusText = $"Could not reach the service: {ex.Message}";
                IsLoading = false;
            });
        }
    }

    private void Populate(IEnumerable<Profile> profiles, IReadOnlyList<Rule> rules, bool demo)
    {
        AllRules = rules;
        var nameById = rules.ToDictionary(r => r.Id, r => string.IsNullOrWhiteSpace(r.Name) ? $"Rule {r.Id}" : r.Name);

        Profiles.Clear();
        foreach (var p in profiles)
        {
            Profiles.Add(new ProfileRowViewModel(p, nameById));
        }
        IsUnavailable = false;
        IsEmpty = Profiles.Count == 0;
        HasLoaded = true;
        StatusText = Profiles.Count == 0
            ? "No profiles yet."
            : $"{Profiles.Count} profile{(Profiles.Count == 1 ? "" : "s")}" + (demo ? " (demo data)." : ".");
        IsLoading = false;
    }

    /// <summary>Resolves member rule ids to display names for confirmation UIs.</summary>
    public IReadOnlyList<string> MemberRuleNames(IEnumerable<long> ruleIds)
    {
        var nameById = AllRules.ToDictionary(r => r.Id, r => string.IsNullOrWhiteSpace(r.Name) ? $"Rule {r.Id}" : r.Name);
        return ruleIds.Select(id => nameById.TryGetValue(id, out var n) ? n : $"Rule {id}").ToList();
    }

    public async Task<(bool ok, string message)> SetActiveAsync(long profileId, bool active)
    {
        if (_fake)
        {
            var row = Profiles.FirstOrDefault(p => p.Id == profileId);
            row?.ApplyActive(active);
            return (true, string.Empty);
        }

        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.SetProfileActiveAsync(profileId, active).ConfigureAwait(false);
            if (!outcome.Supported)
            {
                return (false, "This service is too old to change profiles.");
            }
            if (!outcome.Value.Ok)
            {
                return (false, string.IsNullOrEmpty(outcome.Value.Message)
                    ? "The service could not change this profile."
                    : outcome.Value.Message);
            }
            return (true, string.Empty);
        }
        catch (Exception ex)
        {
            return (false, ex.Message);
        }
    }

    public async Task<(bool ok, string message)> DeleteProfileAsync(long profileId)
    {
        if (_fake)
        {
            var row = Profiles.FirstOrDefault(p => p.Id == profileId);
            if (row is not null)
            {
                Post(() => Profiles.Remove(row));
            }
            return (true, string.Empty);
        }

        try
        {
            using var channel = AtlasChannel.Connect(_who);
            var outcome = await channel.DeleteProfileAsync(profileId).ConfigureAwait(false);
            if (!outcome.Supported)
            {
                return (false, "This service is too old to delete profiles.");
            }
            return outcome.Value.Ok ? (true, string.Empty) : (false, "The service could not delete this profile.");
        }
        catch (Exception ex)
        {
            return (false, ex.Message);
        }
    }

    private void LoadFake()
    {
        Post(() => Populate(RulesDemoData.SampleProfiles(), RulesDemoData.SampleRules(), demo: true));
    }

    public void Stop() => _cts?.Cancel();

    private void Post(Action action) => _dispatcher.TryEnqueue(() => action());
}

/// <summary>One profile row (pre-formatted): power mode, member rules, active state.</summary>
public sealed partial class ProfileRowViewModel : ObservableObject
{
    private readonly IReadOnlyDictionary<long, string> _nameById;

    [ObservableProperty] private bool _active;

    public ProfileRowViewModel(Profile profile, IReadOnlyDictionary<long, string> nameById)
    {
        Profile = profile;
        _nameById = nameById;
        _active = profile.Active;
    }

    public Profile Profile { get; }

    public long Id => Profile.Id;
    public string Name => string.IsNullOrWhiteSpace(Profile.Name) ? "(unnamed profile)" : Profile.Name;
    public string PowerModeLabel => RulesFormatter.PowerModeLabel(Profile.PowerMode);

    public string MemberSummary
    {
        get
        {
            if (Profile.RuleIds.Count == 0)
            {
                return "No member rules";
            }
            var names = Profile.RuleIds.Select(id => _nameById.TryGetValue(id, out var n) ? n : $"Rule {id}");
            return string.Join(", ", names);
        }
    }

    public void ApplyActive(bool active)
    {
        Profile.Active = active;
        Active = active;
    }
}
