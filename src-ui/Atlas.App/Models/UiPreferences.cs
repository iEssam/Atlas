namespace Atlas.App.Models;

/// <summary>The user-selected application theme.</summary>
public enum ThemePreference
{
    System,
    Light,
    Dark,
}

/// <summary>The default amount of technical evidence shown across the UI.</summary>
public enum DetailLevel
{
    Simple,
    Detailed,
    Expert,
}

/// <summary>
/// Non-sensitive UI preferences. Search text and automatically collected
/// evidence are never persisted here. Tracked applications are explicit user
/// choices and contain image names only, never paths, command lines, or samples.
/// </summary>
public sealed class UiPreferences
{
    public ThemePreference Theme { get; set; } = ThemePreference.System;

    public DetailLevel DetailLevel { get; set; } = DetailLevel.Detailed;

    public List<string> TrackedApplications { get; set; } = new();
}
