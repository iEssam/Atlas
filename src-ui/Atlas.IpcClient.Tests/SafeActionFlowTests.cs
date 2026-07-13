using System.Threading.Tasks;
using Atlas.IpcClient;
using Atlas.V0;
using Xunit;

namespace Atlas.IpcClient.Tests;

public class SafeActionFlowTests
{
    private static SafeActionFlow Flow(IActionBroker broker,
        ProcessActionKind action = ProcessActionKind.Suspend) =>
        new(broker, pid: 4242, createTime100ns: 123456789, action);

    [Fact]
    public async Task Allowed_Prepare_ArmsExecute_AndFlowsTokenThrough()
    {
        var risk = new ActionRisk { VisibleWindows = 1 };
        risk.Notes.Add("has one visible window");
        var broker = FakeActionBroker.Allowing(risk, token: "tok-42");
        var flow = Flow(broker);

        await flow.PrepareAsync();

        Assert.Equal(SafeActionPhase.Allowed, flow.Phase);
        Assert.True(flow.CanExecute);
        Assert.NotNull(flow.Risk);
        Assert.Contains("visible window", flow.RiskSummary);

        await flow.ExecuteAsync();

        Assert.Equal(SafeActionPhase.Completed, flow.Phase);
        Assert.True(flow.Succeeded);
        Assert.Equal("tok-42", broker.LastExecutedToken); // token flowed to execute
        Assert.Equal(1, broker.ExecuteCallCount);
    }

    [Fact]
    public async Task Denied_Prepare_OffersNoExecute()
    {
        var broker = FakeActionBroker.Denying("Protected critical process.");
        var flow = Flow(broker, ProcessActionKind.Terminate);

        await flow.PrepareAsync();

        Assert.Equal(SafeActionPhase.Denied, flow.Phase);
        Assert.False(flow.CanExecute);
        Assert.Equal("Protected critical process.", flow.DenialReason);

        // Execute must be a no-op when denied — the broker is never called.
        await flow.ExecuteAsync();
        Assert.Equal(0, broker.ExecuteCallCount);
        Assert.Equal(SafeActionPhase.Denied, flow.Phase);
    }

    [Fact]
    public async Task Unsupported_Server_DegradesGracefully()
    {
        var broker = FakeActionBroker.Unsupported();
        var flow = Flow(broker);

        await flow.PrepareAsync();

        Assert.Equal(SafeActionPhase.Unsupported, flow.Phase);
        Assert.False(flow.CanExecute);
        Assert.Contains("too old", flow.ResultMessage);
    }

    [Fact]
    public async Task Execute_IsSingleUse_SecondCallNoOps()
    {
        var broker = FakeActionBroker.Allowing();
        var flow = Flow(broker);
        await flow.PrepareAsync();

        await flow.ExecuteAsync();
        await flow.ExecuteAsync(); // token consumed; must not fire again

        Assert.Equal(1, broker.ExecuteCallCount);
    }

    [Fact]
    public async Task Allowed_ButNoToken_TreatedAsDenied()
    {
        // A broker that claims allowed but omits a token must never execute.
        var broker = FakeActionBroker.Allowing(token: string.Empty);
        var flow = Flow(broker);

        await flow.PrepareAsync();

        Assert.Equal(SafeActionPhase.Denied, flow.Phase);
        Assert.False(flow.CanExecute);
        await flow.ExecuteAsync();
        Assert.Equal(0, broker.ExecuteCallCount);
    }

    [Fact]
    public async Task ExecuteFailure_ReportsMessage_WithoutSuccess()
    {
        var broker = FakeActionBroker.Allowing(
            executeSuccess: false, executeMessage: "Access denied.");
        var flow = Flow(broker);
        await flow.PrepareAsync();

        await flow.ExecuteAsync();

        Assert.Equal(SafeActionPhase.Completed, flow.Phase);
        Assert.False(flow.Succeeded);
        Assert.Equal("Access denied.", flow.ResultMessage);
    }

    [Fact]
    public void Verbs_MatchAction()
    {
        Assert.Equal("End", Flow(FakeActionBroker.Allowing(), ProcessActionKind.Terminate).ActionVerb);
        Assert.Equal("Suspend", Flow(FakeActionBroker.Allowing(), ProcessActionKind.Suspend).ActionVerb);
        Assert.Contains("not reversible",
            Flow(FakeActionBroker.Allowing(), ProcessActionKind.Terminate).ReversibilityText);
    }
}
