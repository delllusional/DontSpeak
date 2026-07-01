using System;
using Xunit;

namespace DontSpeak.Tests;

/// <summary>
/// The model-status parse (<see cref="HealthSnapshot.FromJson(string, Func{string, double, string, string})"/>)
/// with a stubbed state-word formatter — the same wire shapes dontspeakd's
/// model_status_json emits, mirroring the macOS DontSpeakLogic tests.
/// </summary>
public class HealthSnapshotTests
{
    /// <summary>Stub for the Rust `ds_engine_state_word` formatter (tests run without ds_core.dll).</summary>
    private static string Word(string state, double progress, string why) => state;

    private static HealthSnapshot Parse(string json) => HealthSnapshot.FromJson(json, Word);

    // ── The engine-down / garbage paths must yield the safe default snapshot ──

    [Theory]
    [InlineData("")]
    [InlineData("   ")]
    [InlineData("{}")]
    [InlineData("not json at all")]
    [InlineData("[1,2,3]")]
    public void EmptyOrMalformedPayloadIsTheDefaultSnapshot(string json)
    {
        var s = Parse(json);
        Assert.False(s.Activity.EngineRunning);
        Assert.Equal(0UL, s.StatusSeq);
        Assert.Equal(new[] { "stt", "tts_animated" }, s.Activity.TrayIndicator);
    }

    // ── Happy path: a well-formed payload maps every group ──

    [Fact]
    public void WellFormedPayloadMapsActivityAndSeq()
    {
        var s = Parse("""
            {"seq": 42, "running": {"caps": true, "stt_active": true, "tts_active": false, "muted": true}}
            """);
        Assert.True(s.Activity.EngineRunning);   // any well-formed payload ⇒ engine is up
        Assert.Equal(42UL, s.StatusSeq);
        Assert.True(s.Activity.Caps);
        Assert.True(s.Activity.Recording);
        Assert.False(s.Activity.Speaking);
        Assert.True(s.Activity.Muted);
    }

    /// <summary>An absent tray_indicator keeps the {"stt","tts_animated"} default; a present
    /// one replaces it (nulls dropped); an empty array means "never tint".</summary>
    [Fact]
    public void TrayIndicatorOverridesOnlyWhenPresent()
    {
        Assert.Equal(
            new[] { "stt", "tts_animated" },
            Parse("""{"seq": 1}""").Activity.TrayIndicator);
        Assert.Equal(
            new[] { "tts" },
            Parse("""{"tray_indicator": ["tts", null]}""").Activity.TrayIndicator);
        Assert.Empty(Parse("""{"tray_indicator": []}""").Activity.TrayIndicator);
    }

    /// <summary>dictation.has_paste_target FAILS OPEN: absent reads as true (the overlay
    /// must not warn "no target" just because an old engine omits the key).</summary>
    [Fact]
    public void DictationHasTargetFailsOpen()
    {
        Assert.True(Parse("""{"dictation": {"text": "hi"}}""").Dictation.DictHasTarget);
        Assert.False(Parse("""{"dictation": {"has_paste_target": false}}""").Dictation.DictHasTarget);
        var d = Parse("""{"dictation": {"text": "hello", "awaiting_confirm": true, "local_stt": true}}""").Dictation;
        Assert.Equal("hello", d.DictText);
        Assert.True(d.DictAwaitingConfirm);
        Assert.True(d.DictLocalStt);
    }

    /// <summary>Missing/empty engine tokens fall to each engine's own default so a partial
    /// payload still picks a row to render.</summary>
    [Fact]
    public void EngineSelectionFallsBackPerEngine()
    {
        var s = Parse("""{"seq": 1}""");
        Assert.Equal("claude_code", s.EngineSelection.SttEngine);
        Assert.Equal("built_in", s.EngineSelection.TtsEngine);
        var t = Parse("""{"stt_engine": "built_in", "tts_engine": "system", "tts_provider": "coreml"}""");
        Assert.Equal("built_in", t.EngineSelection.SttEngine);
        Assert.Equal("system", t.EngineSelection.TtsEngine);
        Assert.Equal("coreml", t.EngineSelection.TtsProvider);
    }

    /// <summary>The engine `state` string drives the enum 1:1; a missing object reads as
    /// Missing; an unknown state falls to Missing (never throws on a newer engine).</summary>
    [Theory]
    [InlineData("running", EngineState.Running)]
    [InlineData("idle", EngineState.Idle)]
    [InlineData("warming", EngineState.Warming)]
    [InlineData("failed", EngineState.Failed)]
    [InlineData("downloading", EngineState.Downloading)]
    [InlineData("something_new", EngineState.Missing)]
    public void EngineStateStringMapsToEnum(string state, EngineState expected)
    {
        // The space before the final brace keeps the JSON's `}}` from reading as the
        // $$-interpolation's closing delimiter (CS9007).
        var s = Parse($$"""{"kokoro": {"state": "{{state}}", "progress": 0.5} }""");
        Assert.Equal(expected, s.EngineDots.Kokoro.State);
        Assert.Equal(0.5, s.EngineDots.Kokoro.Progress);
        Assert.Equal(EngineState.Missing, s.EngineDots.Parakeet.State); // absent object
    }

    [Fact]
    public void StatsBlocksMapIntoTheSnapshotGroups()
    {
        var s = Parse("""
            {"stats": {
               "tts": {"rtf_avg": 1.2, "rtf_min": 1.0, "rtf_max": 1.5, "utterances": 7, "audio_secs": 33.5, "failures": 2},
               "stt": {"rtf_avg": 0.4, "transcriptions": 3, "audio_secs": 9.0},
               "lifetime": {"tts_secs": 100.5, "stt_secs": 50.25}}}
            """);
        Assert.Equal(1.2, s.Tts.RtfAvg);
        Assert.Equal(7, s.Tts.Utterances);
        Assert.Equal(2, s.Tts.Failures);
        Assert.Equal(3, s.Stt.Transcriptions);
        Assert.Equal(9.0, s.Stt.AudioSecs);
        Assert.Equal(100.5, s.Lifetime.TtsSecs);
        Assert.Equal(50.25, s.Lifetime.SttSecs);
    }

    // ── IndicatorState: the ONE tray/state-stripe mapping, gated by tray_indicator ──

    /// <summary>Recording wins over speaking; each state tints only when its token (plain or
    /// _animated) is in the set; an empty set never tints.</summary>
    [Fact]
    public void IndicatorStateHonorsTheTrayIndicatorSet()
    {
        var s = new HealthSnapshot();
        s.Activity.Recording = true;
        s.Activity.Speaking = true;
        Assert.Equal(TrayIcon.IconState.Recording, s.IndicatorState()); // stt in the default set

        s.Activity.TrayIndicator = new[] { "tts_animated" };
        Assert.Equal(TrayIcon.IconState.Speaking, s.IndicatorState()); // stt not in set → tts wins

        s.Activity.TrayIndicator = Array.Empty<string>();
        Assert.Equal(TrayIcon.IconState.Idle, s.IndicatorState()); // never tint

        s.Activity.Recording = false;
        s.Activity.Speaking = false;
        s.Activity.TrayIndicator = new[] { "stt", "tts" };
        Assert.Equal(TrayIcon.IconState.Idle, s.IndicatorState()); // nothing active
    }
}
