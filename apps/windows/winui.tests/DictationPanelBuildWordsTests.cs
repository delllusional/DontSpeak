using Xunit;

namespace DontSpeak.Tests;

/// <summary>
/// <see cref="DictationPanel.BuildWords"/> — the per-word fade-in/fade-out diff that drives the
/// dictation overlay's blur transitions. `now` is passed in directly (never a live
/// <see cref="System.Diagnostics.Stopwatch"/>) and no <see cref="DictationPanel"/> instance is
/// constructed (that would open a real Win32 window), so this runs as plain data-in/data-out
/// logic, mirroring <see cref="HealthSnapshotTests"/> and <see cref="LogParserTests"/>.
/// </summary>
public class DictationPanelBuildWordsTests
{
    // CA1861: constant array arguments hoisted out of the repeatedly-built snapshots/asserts
    // (same convention as HealthSnapshotTests).
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
        // The stable prefix keeps its original stamp so it doesn't re-animate on a partial.
        Assert.Equal(Stamp1000Then5000, next.AppearMs);
    }

    [Fact]
    public void ReplacedWordBlursOutAtTheSameSlot()
    {
        var prev = Prev(HelloWorld, Stamp1000Twice);
        var next = new DictationPanel.Snapshot();

        // A refinement: "world" → "there" at slot 1 (e.g. STT correcting itself).
        DictationPanel.BuildWords(prev, next, "hello there", now: 2000);

        Assert.Equal(HelloThere, next.Words);
        Assert.Equal(Stamp1000Then2000, next.AppearMs); // hello unchanged, there is new
        Assert.Equal(NullThenWorld, next.OutWords); // the old word fades out
        Assert.Equal(ZeroThen2000, next.OutAppearMs);
    }

    [Fact]
    public void InFlightOutgoingFadeCarriesOverOnAnUnchangedRapidRerender()
    {
        // Right after a replacement: slot 1 is mid-fade-out ("world" fading, stamped at 2000).
        var prev = Prev(HelloThere, Stamp1000Then2000, NullThenWorld, ZeroThen2000);
        var next = new DictationPanel.Snapshot();

        // Same text, 100ms later — well inside the 360ms fade window.
        DictationPanel.BuildWords(prev, next, "hello there", now: 2100);

        Assert.Equal(NullThenWorld, next.OutWords);
        Assert.Equal(ZeroThen2000, next.OutAppearMs);
        Assert.Equal(Stamp1000Then2000, next.AppearMs); // unchanged slots keep their stamp
    }

    [Fact]
    public void ExpiredOutgoingFadeIsNotCarriedOver()
    {
        var prev = Prev(HelloThere, Stamp1000Then2000, NullThenWorld, ZeroThen2000);
        var next = new DictationPanel.Snapshot();

        // 500ms later — past the 360ms fade window, so the stale outgoing word is dropped.
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
