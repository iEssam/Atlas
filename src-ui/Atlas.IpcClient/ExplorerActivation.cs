namespace Atlas.IpcClient;

/// <summary>
/// A validated request from the Explorer shell command. Parsing is deliberately
/// independent of WinUI so the command-line contract can be tested without
/// starting the desktop application.
/// </summary>
public sealed record ExplorerActivation(string FilePath)
{
    public const string FindUsingOption = "--find-using";

    public static ExplorerActivation? Parse(IEnumerable<string> arguments)
    {
        ArgumentNullException.ThrowIfNull(arguments);

        using var iterator = arguments.GetEnumerator();
        while (iterator.MoveNext())
        {
            var argument = iterator.Current;
            if (string.Equals(argument, FindUsingOption, StringComparison.OrdinalIgnoreCase))
            {
                return iterator.MoveNext() ? FromPath(iterator.Current) : null;
            }

            var prefix = FindUsingOption + "=";
            if (argument?.StartsWith(prefix, StringComparison.OrdinalIgnoreCase) == true)
            {
                return FromPath(argument[prefix.Length..]);
            }
        }

        return null;
    }

    private static ExplorerActivation? FromPath(string? value)
    {
        var path = value?.Trim();
        if (string.IsNullOrEmpty(path) || !Path.IsPathFullyQualified(path))
        {
            return null;
        }

        return new ExplorerActivation(path);
    }
}
