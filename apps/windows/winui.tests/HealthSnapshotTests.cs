using System;
using Xunit;

namespace DontSpeak.Tests;

/// <summary>
/// Model-status parse with stubbed state-word formatter (no ds_core.dll) — wire shapes match
/// dontspeakd model_status_json / macOS DontSpeakLogic tests.
/// </summary>
public class HealthSnapshotTests
{
    /// <summary>Stub for ds_engine_state_word (tests without ds_core.dll).</summary>
    private static string Word(string state, double progress, string why) => state;

    private static HealthSnapshot Parse(string json) => HealthSnapshot.FromJson(json, Word);

    // CA1861: hoist constant arrays out of asserts.
    private static readonly string[] DefaultIndicator = { "stt", "tts_animated" };
    private static readonly string[] TtsOnly = { "tts" };
    private static readonly string[] AlexOnly = { "Alex" };

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
        Assert.Equal(DefaultIndicator, s.Activity.TrayIndicator);
    }

    [Fact]
    public void WellFormedPayloadMapsActivityAndSeq()
    {
        var s = Parse("""
            {"seq": 42, "running": {"caps": true, "stt_active": true, "tts_active": false, "muted": true}}
            """);
        Assert.True(s.Activity.EngineRunning);
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
            DefaultIndicator,
            Parse("""{"seq": 1}""").Activity.TrayIndicator);
        Assert.Equal(
            TtsOnly,
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

    /// <summary>dictation.refused FAILS QUIET: absent (an older engine) reads as false — no
    /// spurious refusal glow — while an explicit true surfaces the refused-start cue.</summary>
    [Fact]
    public void DictationRefusedFailsQuiet()
    {
        Assert.False(Parse("""{"dictation": {"text": "hi"}}""").Dictation.DictRefused);
        Assert.True(Parse("""{"dictation": {"refused": true}}""").Dictation.DictRefused);
    }

    /// <summary>The canonical dictation.state token parses through; absent (older engine)
    /// reads as "" — the ShowPanel fallback signal.</summary>
    [Fact]
    public void DictationStateParsesAndDefaultsEmpty()
    {
        Assert.Equal("recording", Parse("""{"dictation": {"state": "recording"}}""").Dictation.DictState);
        Assert.Equal("", Parse("""{"dictation": {"text": "hi"}}""").Dictation.DictState);
    }

    /// <summary>ShowPanel switches on the canonical token (vocabulary:
    /// rust/crates/ds-status/src/dictation_state.rs): hidden ⇒ false, the three visible
    /// states ⇒ true, regardless of what the legacy booleans say.</summary>
    [Theory]
    [InlineData("hidden", false)]
    [InlineData("recording", true)]
    [InlineData("awaiting_confirm", true)]
    [InlineData("refused", true)]
    public void ShowPanelFollowsTheCanonicalToken(string token, bool expected)
    {
        // Legacy booleans contradict token so token wins.
        var visible = new Dictation { DictState = token };
        Assert.Equal(expected, visible.ShowPanel(recording: false));
        var hidden = new Dictation
        {
            DictState = token,
            DictAwaitingConfirm = true,
            DictLocalStt = true,
            DictRefused = true,
        };
        Assert.Equal(expected, hidden.ShowPanel(recording: true));
    }

    /// <summary>An absent/unknown token (older engine DLL) falls back to the legacy boolean
    /// derivation — a showing case and a hidden case each way, so skew can't kill the panel.</summary>
    [Theory]
    [InlineData("")]
    [InlineData("bogus_token")]
    public void ShowPanelFallsBackToBooleansOnUnknownToken(string token)
    {
        Assert.True(new Dictation { DictState = token, DictAwaitingConfirm = true }.ShowPanel(recording: false));
        Assert.True(new Dictation { DictState = token, DictLocalStt = true }.ShowPanel(recording: true));
        Assert.False(new Dictation { DictState = token, DictLocalStt = false }.ShowPanel(recording: true));
        Assert.True(new Dictation { DictState = token, DictRefused = true }.ShowPanel(recording: false));
        Assert.False(new Dictation { DictState = token }.ShowPanel(recording: false));
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
    [InlineData("blocked", EngineState.Blocked)]
    [InlineData("failed", EngineState.Failed)]
    [InlineData("downloading", EngineState.Downloading)]
    [InlineData("something_new", EngineState.Missing)]
    public void EngineStateStringMapsToEnum(string state, EngineState expected)
    {
        // Space before final brace: avoid $$ interpolation treating `}}` as closer (CS9007).
        var s = Parse($$"""{"kokoro": {"state": "{{state}}", "progress": 0.5} }""");
        Assert.Equal(expected, s.EngineDots.Kokoro.State);
        Assert.Equal(0.5, s.EngineDots.Kokoro.Progress);
        Assert.Equal(EngineState.Missing, s.EngineDots.Parakeet.State);
    }

    /// <summary>diarization engine object uses same ToEngine mapping as Kokoro/etc.</summary>
    [Fact]
    public void DiarizationEngineObjectMapsIntoEngineDots()
    {
        var s = Parse("""{"diarization": {"state": "running", "progress": 1.0}}""");
        Assert.Equal(EngineState.Running, s.EngineDots.Diarization.State);
        Assert.Equal(1.0, s.EngineDots.Diarization.Progress);
    }

    [Fact]
    public void StatsBlocksMapIntoTheSnapshotGroups()
    {
        var s = Parse("""
            {"stats": {
               "tts": {"rtf_avg": 1.2, "rtf_min": 1.0, "rtf_max": 1.5, "utterances": 7, "audio_secs": 33.5, "failures": 2},
               "stt": {"rtf_avg": 0.4, "transcriptions": 3, "audio_secs": 9.0, "failures": 1},
               "lifetime": {"tts_secs": 100, "stt_secs": 50},
               "diarization": {"enabled": true, "speakers": ["Alex"], "clustering_threshold": 0.72, "runtime": "ane"}}}
            """);
        Assert.Equal(1.2, s.Tts.RtfAvg);
        Assert.Equal(7, s.Tts.Utterances);
        Assert.Equal(2, s.Tts.Failures);
        Assert.Equal(3, s.Stt.Transcriptions);
        Assert.Equal(9.0, s.Stt.AudioSecs);
        Assert.Equal(1, s.Stt.Failures);
        Assert.Equal(100, s.Lifetime.TtsSecs);
        Assert.Equal(50, s.Lifetime.SttSecs);
        Assert.True(s.Diarization.Enabled);
        Assert.Equal(AlexOnly, s.Diarization.Speakers);
        Assert.Equal(0.72, s.Diarization.ClusteringThreshold);
        Assert.Equal("ane", s.Diarization.Runtime);
    }

    /// <summary>Recording wins over speaking; tint only when token/_animated in set; [] never tints.</summary>
    [Fact]
    public void IndicatorStateHonorsTheTrayIndicatorSet()
    {
        var s = new HealthSnapshot();
        s.Activity.Recording = true;
        s.Activity.Speaking = true;
        Assert.Equal(TrayIcon.IconState.Recording, s.IndicatorState());

        s.Activity.TrayIndicator = new[] { "tts_animated" };
        Assert.Equal(TrayIcon.IconState.Speaking, s.IndicatorState());

        s.Activity.TrayIndicator = Array.Empty<string>();
        Assert.Equal(TrayIcon.IconState.Idle, s.IndicatorState());

        s.Activity.Recording = false;
        s.Activity.Speaking = false;
        s.Activity.TrayIndicator = new[] { "stt", "tts" };
        Assert.Equal(TrayIcon.IconState.Idle, s.IndicatorState());
    }
}
