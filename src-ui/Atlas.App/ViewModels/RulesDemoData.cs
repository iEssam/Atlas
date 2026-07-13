using System.Collections.Generic;
using Atlas.IpcClient;
using Atlas.V0;

namespace Atlas.App.ViewModels;

/// <summary>
/// Sample rules-engine data so the Rules and Profiles pages can be previewed
/// without the AtlasRules backend (which lands after this UI — task brief). Gated
/// behind <c>ATLAS_FAKE_RULES=1</c>; never used against a live service. The data
/// is representative — a couple of realistic rules, a protected simulation
/// target, and an active intervention — so the whole UX, including the calm
/// "System-critical" framing, can be seen end to end.
/// </summary>
internal static class RulesDemoData
{
    public static IReadOnlyList<Rule> SampleRules() => new List<Rule>
    {
        new Rule
        {
            Id = 1,
            Name = "Tame background Chrome",
            Enabled = true,
            MatchImage = "chrome.exe",
            Trigger = RuleTrigger.WhileRunning,
            Precedence = 10,
            Action = new RuleAction
            {
                Priority = PriorityClass.PriorityBelowNormal,
                AffinityMode = CoreAffinityMode.PreferECores,
                EcoQos = true,
            },
        },
        new Rule
        {
            Id = 2,
            Name = "Prioritise the game",
            Enabled = false,
            MatchImage = "game.exe",
            Trigger = RuleTrigger.OnFullscreen,
            Precedence = 50,
            Action = new RuleAction
            {
                Priority = PriorityClass.PriorityAboveNormal,
                AffinityMode = CoreAffinityMode.PreferPCores,
            },
        },
        new Rule
        {
            Id = 3,
            Name = "Battery-saver for the indexer",
            Enabled = true,
            MatchImage = "searchindexer.exe",
            Trigger = RuleTrigger.OnDcPower,
            Precedence = 20,
            Action = new RuleAction
            {
                Priority = PriorityClass.PriorityIdle,
                EcoQos = true,
            },
        },
    };

    public static IReadOnlyList<Intervention> SampleInterventions(long nowMs) => new List<Intervention>
    {
        new Intervention
        {
            RuleId = 1,
            RuleName = "Tame background Chrome",
            Pid = 4242,
            ImageName = "chrome.exe",
            Applied = "Below Normal, E-cores, Eco",
            SinceMs = nowMs - 12 * 60_000,
        },
        new Intervention
        {
            RuleId = 3,
            RuleName = "Battery-saver for the indexer",
            Pid = 1180,
            ImageName = "searchindexer.exe",
            Applied = "Idle, Eco",
            SinceMs = nowMs - 3 * 60 * 60_000L,
        },
    };

    /// <summary>
    /// A representative simulation for <paramref name="rule"/> in demo mode,
    /// including one protected-critical target that is clearly blocked, so the
    /// preview UX (the centerpiece) can be seen without a backend.
    /// </summary>
    public static SimulateRuleReply SampleSimulation(Rule rule)
    {
        var reply = new SimulateRuleReply();
        var newPriority = RulesFormatter.PriorityClassLabel(rule.Action?.Priority ?? PriorityClass.Unspecified);
        var newAffinity = rule.Action is null
            ? RulesFormatter.Unchanged
            : (rule.Action.AffinityMode == CoreAffinityMode.CustomMask
                ? RulesFormatter.AffinityMaskText(rule.Action.AffinityMask)
                : RulesFormatter.CoreAffinityModeLabel(rule.Action.AffinityMode));
        bool eco = rule.Action?.EcoQos ?? false;

        reply.Targets.Add(new SimulatedTarget
        {
            Pid = 4242,
            ImageName = string.IsNullOrWhiteSpace(rule.MatchImage) ? "sample.exe" : rule.MatchImage,
            CurrentPriority = "Normal",
            NewPriority = newPriority,
            CurrentAffinity = "All cores",
            NewAffinity = newAffinity,
            EcoQosChange = eco,
            Blocked = false,
        });
        reply.Targets.Add(new SimulatedTarget
        {
            Pid = 5560,
            ImageName = string.IsNullOrWhiteSpace(rule.MatchImage) ? "sample.exe" : rule.MatchImage,
            CurrentPriority = "Normal",
            NewPriority = newPriority,
            CurrentAffinity = "All cores",
            NewAffinity = newAffinity,
            EcoQosChange = eco,
            Blocked = false,
        });
        reply.Targets.Add(new SimulatedTarget
        {
            Pid = 4,
            ImageName = "System",
            CurrentPriority = "Normal",
            NewPriority = newPriority,
            Blocked = true,
            BlockedReason = "System-critical — Atlas won't touch this.",
        });
        return reply;
    }

    public static IReadOnlyList<Profile> SampleProfiles() => new List<Profile>
    {
        new Profile
        {
            Id = 1,
            Name = "Gaming",
            PowerMode = "HighPerformance",
            Active = false,
            RuleIds = { 2 },
        },
        new Profile
        {
            Id = 2,
            Name = "Battery saver",
            PowerMode = "PowerSaver",
            Active = true,
            RuleIds = { 1, 3 },
        },
    };
}
