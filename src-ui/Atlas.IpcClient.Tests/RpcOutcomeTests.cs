using System;
using Atlas.IpcClient;
using Xunit;

namespace Atlas.IpcClient.Tests;

public class RpcOutcomeTests
{
    [Fact]
    public void Ok_ExposesValue()
    {
        var outcome = RpcOutcome<int>.Ok(42);
        Assert.True(outcome.Supported);
        Assert.Equal(42, outcome.Value);
        Assert.Equal(42, outcome.ValueOr(0));
        Assert.Null(outcome.UnsupportedReason);
    }

    [Fact]
    public void Unsupported_HidesValue_AndCarriesReason()
    {
        var outcome = RpcOutcome<string>.Unsupported("not implemented");
        Assert.False(outcome.Supported);
        Assert.Equal("not implemented", outcome.UnsupportedReason);
        Assert.Throws<InvalidOperationException>(() => _ = outcome.Value);
    }

    [Fact]
    public void ValueOr_ReturnsFallbackWhenUnsupported()
    {
        var outcome = RpcOutcome<string>.Unsupported("too old");
        Assert.Equal("fallback", outcome.ValueOr("fallback"));
    }
}
