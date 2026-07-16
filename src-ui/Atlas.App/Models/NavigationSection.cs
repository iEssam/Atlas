namespace Atlas.App.Models;

internal sealed record NavigationDestination(string Label, Type PageType);

internal sealed record NavigationSection(
    string Key,
    string Label,
    IReadOnlyList<NavigationDestination> Destinations);
