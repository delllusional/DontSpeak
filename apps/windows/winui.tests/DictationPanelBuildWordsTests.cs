using Xunit;

namespace DontSpeak.Tests;

/// <summary>
/// <see cref="DictationPanel.BuildWords"/> pure data — inject `now`; skip panel instance
/// (would open a Win32 window).
/// </summary>
public class DictationPanelBuildWordsTests
{
    // CA1861: hoist constant arrays out of asserts.
    private static readonly string[] HelloWorld = { "hello", "world" };
    private static readonly string[] HelloThere = { "hello", "there" };
    private static readonly string[] HelloWorldFoo = { "hello", "world", "foo" };
    private static readonly long[] Stamp1000Twice = { 1000, 1000 };
    private static readonly long[] Stamp1000Then5000 = { 1000, 1000, 5000 };
    private static readonly long[] Stamp1000Then2000 = { 1000, 2000 };
    private static readonly string?[] NullTwice = { null, null };
    private static readonly string?[] NullThenWorld = { null, "world" };
    private static readonly long[] ZeroTwice = { 0, 0 };
    private static readonly long[] ZeroThen2000 = { 0, 2000 };

    private static DictationPanel.Snapshot Prev(
        string[] words, long[] appearMs, string?[]? outWords = null, long[]? outAppearMs = null) => new()
    {
        Words = words,
        AppearMs = appearMs,
        OutWords = outWords ?? new string?[words.Length],
        OutAppearMs = outAppearMs ?? new long[words.Length],
    };

    [Fact]
    public void FreshTranscriptStampsEveryWordWithNow()
    {
        var next = new DictationPanel.Snapshot();
        DictationPanel.BuildWords(DictationPanel.Snapshot.Empty, next, "hello world", now: 1000);

        Assert.Equal(HelloWorld, next.Words);
        Assert.Equal(Stamp1000Twice, next.AppearMs);
        Assert.Equal(NullTwice, next.OutWords);
    }

    [Fact]
    public void UnchangedLeadingWordsKeepTheirOriginalAppearTime()
    {
        var prev = Prev(HelloWorld, Stamp1000Twice);
        var next = new DictationPanel.Snapshot();

        DictationPanel.BuildWords(prev, next, "hello world foo", now: 5000);

        Assert.Equal(HelloWorldFoo, next.Words);
        // Stable prefix keeps stamp (partial append must not re-animate).
        Assert.Equal(Stamp1000Then5000, next.AppearMs);
    }

    [Fact]
    public void ReplacedWordBlursOutAtTheSameSlot()
    {
        var prev = Prev(HelloWorld, Stamp1000Twice);
        var next = new DictationPanel.Snapshot();

        DictationPanel.BuildWords(prev, next, "hello there", now: 2000);

        Assert.Equal(HelloThere, next.Words);
        Assert.Equal(Stamp1000Then2000, next.AppearMs);
        Assert.Equal(NullThenWorld, next.OutWords);
        Assert.Equal(ZeroThen2000, next.OutAppearMs);
    }

    [Fact]
    public void InFlightOutgoingFadeCarriesOverOnAnUnchangedRapidRerender()
    {
        // Slot 1 mid-fade-out inside 360ms window.
        var prev = Prev(HelloThere, Stamp1000Then2000, NullThenWorld, ZeroThen2000);
        var next = new DictationPanel.Snapshot();

        DictationPanel.BuildWords(prev, next, "hello there", now: 2100);

        Assert.Equal(NullThenWorld, next.OutWords);
        Assert.Equal(ZeroThen2000, next.OutAppearMs);
        Assert.Equal(Stamp1000Then2000, next.AppearMs);
    }

    [Fact]
    public void ExpiredOutgoingFadeIsNotCarriedOver()
    {
        var prev = Prev(HelloThere, Stamp1000Then2000, NullThenWorld, ZeroThen2000);
        var next = new DictationPanel.Snapshot();

        // Past 360ms fade window → drop stale outgoing.
        DictationPanel.BuildWords(prev, next, "hello there", now: 2500);

        Assert.Equal(NullTwice, next.OutWords);
        Assert.Equal(ZeroTwice, next.OutAppearMs);
    }

    [Fact]
    public void ClearedTranscriptProducesEmptyArraysWithoutThrowing()
    {
        var prev = Prev(HelloWorld, Stamp1000Twice);
        var next = new DictationPanel.Snapshot();

        DictationPanel.BuildWords(prev, next, "", now: 3000);

        Assert.Empty(next.Words);
        Assert.Empty(next.AppearMs);
        Assert.Empty(next.OutWords);
    }
}
