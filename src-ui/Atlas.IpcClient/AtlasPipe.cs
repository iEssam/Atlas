namespace Atlas.IpcClient;

/// <summary>
/// Pipe-name construction that mirrors the Rust server's scheme
/// (<c>atlas-ipc/src/transport.rs</c>). The server builds
/// <c>\\.\pipe\SystemAtlas.dev.&lt;who&gt;</c> where <c>who</c> is the
/// <c>--pipe</c> discriminator (default: the <c>USERNAME</c> env var, or
/// <c>session</c> if empty/missing).
/// </summary>
public static class AtlasPipe
{
    /// <summary>Prefix the Win32 pipe namespace shares with the Rust side.</summary>
    public const string Prefix = @"\\.\pipe\";

    /// <summary>Fixed scope segment: <c>SystemAtlas.dev.</c>.</summary>
    public const string ScopePrefix = "SystemAtlas.dev.";

    /// <summary>
    /// The full Win32 pipe path for a given discriminator, e.g.
    /// <c>\\.\pipe\SystemAtlas.dev.uidev</c>. Matches
    /// <c>atlas_ipc::pipe_name</c>.
    /// </summary>
    public static string FullPath(string who) => $"{Prefix}{ScopePrefix}{who}";

    /// <summary>
    /// The pipe name portion only (no <c>\\.\pipe\</c> prefix), which is what
    /// <see cref="System.IO.Pipes.NamedPipeClientStream"/> expects as its
    /// <c>pipeName</c> argument (the "." server name carries the namespace).
    /// e.g. <c>SystemAtlas.dev.uidev</c>.
    /// </summary>
    public static string PipeName(string who) => $"{ScopePrefix}{who}";

    /// <summary>
    /// Resolves the discriminator the same way the Rust
    /// <c>default_pipe_name</c> does when none is supplied: the
    /// <c>USERNAME</c> environment variable, falling back to <c>session</c>
    /// when it is missing or empty.
    /// </summary>
    public static string DefaultWho()
    {
        var user = Environment.GetEnvironmentVariable("USERNAME");
        return string.IsNullOrEmpty(user) ? "session" : user;
    }

    /// <summary>The default full pipe path (mirrors <c>default_pipe_name</c>).</summary>
    public static string DefaultFullPath() => FullPath(DefaultWho());
}
