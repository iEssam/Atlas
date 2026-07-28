using Atlas.IpcClient;
using Xunit;

namespace Atlas.IpcClient.Tests;

public sealed class ExplorerActivationTests
{
    [Fact]
    public void Parse_ReadsSeparateAbsolutePathArgument()
    {
        var activation = ExplorerActivation.Parse(
            ["--find-using", @"C:\Program Files\System Atlas\sample file.txt"]);

        Assert.Equal(@"C:\Program Files\System Atlas\sample file.txt", activation?.FilePath);
    }

    [Fact]
    public void Parse_ReadsEqualsFormAndOptionCaseInsensitively()
    {
        var activation = ExplorerActivation.Parse(
            [@"--FIND-USING=\\server\share\folder\sample.txt"]);

        Assert.Equal(@"\\server\share\folder\sample.txt", activation?.FilePath);
    }

    [Theory]
    [InlineData()]
    [InlineData("--find-using")]
    [InlineData("--find-using", "relative.txt")]
    [InlineData("--find-using=")]
    public void Parse_RejectsMissingOrRelativePaths(params string[] arguments)
    {
        Assert.Null(ExplorerActivation.Parse(arguments));
    }

    [Fact]
    public void Parse_IgnoresUnrelatedArguments()
    {
        Assert.Null(ExplorerActivation.Parse(["--diagnostics", @"C:\sample.txt"]));
    }
}
