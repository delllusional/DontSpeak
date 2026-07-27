using System;
using System.Threading;
using System.Threading.Tasks;
using Xunit;

namespace DontSpeak.Tests;

public class StatusPushStateTests
{
    private static string Word(string state, double progress, string why) => state;

    [Fact]
    public void EngineDownIsDeliveredOnceAndRetainsTheLastSequence()
    {
        var state = new StatusPushState(Word);
        var running = state.Accept("""{"seq":42,"activity":{"recording":true}}""");
        Assert.True(running.Snapshot!.Activity.EngineRunning);
        Assert.False(running.Pace);
        Assert.Equal(42UL, state.Since);

        var down = state.Accept("{}");
        Assert.NotNull(down.Snapshot);
        Assert.False(down.Snapshot!.Activity.EngineRunning);
        Assert.True(down.Pace);
        Assert.Equal(42UL, state.Since);

        var stillDown = state.Accept("  {}  ");
        Assert.Null(stillDown.Snapshot);
        Assert.True(stillDown.Pace);
        Assert.Equal(42UL, state.Since);
    }

    [Fact]
    public void MalformedPayloadIsPacedWithoutReplacingLastGoodStatusOrSequence()
    {
        var state = new StatusPushState(Word);
        _ = state.Accept("""{"seq":9,"activity":{"speaking":true}}""");

        var malformed = state.Accept("not json");

        Assert.Null(malformed.Snapshot);
        Assert.True(malformed.Pace);
        Assert.Equal(9UL, state.Since);
    }

    [Fact]
    public void RecoveryAfterEngineDownIsDeliveredEvenWhenSequenceMatches()
    {
        var state = new StatusPushState(Word);
        _ = state.Accept("""{"seq":7}""");
        _ = state.Accept("{}");

        var recovered = state.Accept("""{"seq":7}""");

        Assert.NotNull(recovered.Snapshot);
        Assert.True(recovered.Snapshot!.Activity.EngineRunning);
        Assert.False(recovered.Pace);
    }

    [Fact]
    public void UnchangedRunningStatusIsNotRedelivered()
    {
        var state = new StatusPushState(Word);
        _ = state.Accept("""{"seq":3}""");

        var unchanged = state.Accept("""{"seq":3}""");

        Assert.Null(unchanged.Snapshot);
        Assert.False(unchanged.Pace);
    }

    [Fact]
    public async Task TrayCommandRunsBlockingWorkAwayFromTheCaller()
    {
        int callerThread = Environment.CurrentManagedThreadId;
        using var started = new ManualResetEventSlim();
        using var release = new ManualResetEventSlim();
        var task = TrayCommand.RunAsync(() =>
        {
            Assert.NotEqual(callerThread, Environment.CurrentManagedThreadId);
            started.Set();
            Assert.True(release.Wait(TimeSpan.FromSeconds(5)));
            return true;
        });

        Assert.True(started.Wait(TimeSpan.FromSeconds(5)));
        Assert.False(task.IsCompleted);
        release.Set();
        Assert.True(await task.WaitAsync(TimeSpan.FromSeconds(5)));
    }
}
