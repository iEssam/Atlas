using Atlas.IpcClient;
using Xunit;

namespace Atlas.IpcClient.Tests;

public class AtlasPipeTests
{
    // These assertions pin the C# side to the Rust format in
    // atlas-ipc/src/transport.rs: `\\.\pipe\SystemAtlas.dev.<who>`.

    [Fact]
    public void FullPath_MatchesRustPipeName()
    {
        // Rust: pipe_name("abc") == r"\\.\pipe\SystemAtlas.dev.abc"
        Assert.Equal(@"\\.\pipe\SystemAtlas.dev.abc", AtlasPipe.FullPath("abc"));
    }

    [Fact]
    public void FullPath_ForUidevDiscriminator()
    {
        // The discriminator used by this milestone's live proof.
        Assert.Equal(@"\\.\pipe\SystemAtlas.dev.uidev", AtlasPipe.FullPath("uidev"));
    }

    [Fact]
    public void PipeName_OmitsWin32Prefix()
    {
        // NamedPipeClientStream takes the name without the \\.\pipe\ prefix.
        Assert.Equal("SystemAtlas.dev.uidev", AtlasPipe.PipeName("uidev"));
    }

    [Fact]
    public void FullPath_IsPrefixPlusPipeName()
    {
        var who = "someuser";
        Assert.Equal(AtlasPipe.Prefix + AtlasPipe.PipeName(who), AtlasPipe.FullPath(who));
    }

    [Fact]
    public void DefaultWho_UsesUsernameEnvVar()
    {
        var original = Environment.GetEnvironmentVariable("USERNAME");
        try
        {
            Environment.SetEnvironmentVariable("USERNAME", "alice");
            Assert.Equal("alice", AtlasPipe.DefaultWho());
            Assert.Equal(@"\\.\pipe\SystemAtlas.dev.alice", AtlasPipe.DefaultFullPath());
        }
        finally
        {
            Environment.SetEnvironmentVariable("USERNAME", original);
        }
    }

    [Fact]
    public void DefaultWho_FallsBackToSession_WhenUsernameMissing()
    {
        // Rust: default_pipe_name falls back to "session" when USERNAME is
        // missing or empty.
        var original = Environment.GetEnvironmentVariable("USERNAME");
        try
        {
            Environment.SetEnvironmentVariable("USERNAME", "");
            Assert.Equal("session", AtlasPipe.DefaultWho());
        }
        finally
        {
            Environment.SetEnvironmentVariable("USERNAME", original);
        }
    }
}
