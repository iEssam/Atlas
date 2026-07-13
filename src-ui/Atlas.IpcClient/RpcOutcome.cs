namespace Atlas.IpcClient;

/// <summary>
/// A typed wrapper over an RPC that the server may not implement yet. The M6
/// history/search/action RPCs land server-side after the UI; against an older
/// service they return <c>StatusCode.Unimplemented</c>. Pages branch on
/// <see cref="Supported"/> to render a graceful "history unavailable — server
/// too old" placeholder instead of crashing (task brief; PRD §5 degraded mode).
///
/// <para>
/// This deliberately does <b>not</b> swallow other faults: transport errors,
/// cancellation, and genuine server errors still throw, so real problems stay
/// visible. Only <c>Unimplemented</c> is mapped to <see cref="Unsupported"/>.
/// </para>
/// </summary>
/// <typeparam name="T">The reply payload type.</typeparam>
public readonly struct RpcOutcome<T>
{
    private RpcOutcome(bool supported, T? value, string? unsupportedReason)
    {
        Supported = supported;
        _value = value;
        UnsupportedReason = unsupportedReason;
    }

    private readonly T? _value;

    /// <summary>
    /// True when the server answered the call. False when the server returned
    /// <c>Unimplemented</c> (too old to serve this RPC).
    /// </summary>
    public bool Supported { get; }

    /// <summary>The server's status detail when <see cref="Supported"/> is false.</summary>
    public string? UnsupportedReason { get; }

    /// <summary>The reply payload. Only valid when <see cref="Supported"/> is true.</summary>
    public T Value =>
        Supported
            ? _value!
            : throw new InvalidOperationException(
                "RPC is not supported by the server; check Supported before reading Value.");

    /// <summary>The payload, or <paramref name="fallback"/> when unsupported.</summary>
    public T ValueOr(T fallback) => Supported ? _value! : fallback;

    public static RpcOutcome<T> Ok(T value) => new(true, value, null);

    public static RpcOutcome<T> Unsupported(string reason) => new(false, default, reason);
}
