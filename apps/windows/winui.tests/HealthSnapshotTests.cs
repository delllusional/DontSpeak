using System;
using Xunit;

namespace DontSpeak.Tests;

/// <summary>Canonical model-status parsing with a stubbed formatter (no ds_core.dll).</summary>
public class HealthSnapshotTests
{
    private static string Word(string state, double progress, string why) => state;
    private static HealthSnapshot Parse(string json) => HealthSnapshot.FromJson(json, Word);

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
    public void ActivityAndSequenceMapTogether()
    {
        var s = Parse("""
            {"seq":42,"activity":{"caps":true,"caps_active":true,
             "recording":true,"speaking":false,"speaker":null,"muted":true}}
            """);
        Assert.True(s.Activity.EngineRunning);
        Assert.Equal(42UL, s.StatusSeq);
        Assert.True(s.Activity.CapsEnabled);
        Assert.True(s.Activity.CapsActive);
        Assert.True(s.Activity.Recording);
        Assert.False(s.Activity.Speaking);
        Assert.Null(s.Activity.Speaker);
        Assert.True(s.Activity.Muted);
    }

    [Fact]
    public void SpeakerIsClearedWhenIdle()
    {
        var speaking = Parse("""{"activity":{"speaking":true,"speaker":"claude_code"}}""");
        Assert.Equal("claude_code", speaking.Activity.Speaker);

        var idle = Parse("""{"activity":{"speaking":false,"speaker":"claude_code"}}""");
        Assert.Null(idle.Activity.Speaker);
    }

    [Fact]
    public void TrayIndicatorReplacesTheDefault()
    {
        Assert.Equal(DefaultIndicator, Parse("""{"seq":1}""").Activity.TrayIndicator);
        Assert.Equal(TtsOnly, Parse("""{"tray":["tts"]}""").Activity.TrayIndicator);
        Assert.Empty(Parse("""{"tray":[]}""").Activity.TrayIndicator);
    }

    [Theory]
    [InlineData("hidden", false)]
    [InlineData("recording", true)]
    [InlineData("awaiting_confirm", true)]
    [InlineData("refused", true)]
    public void DictationUsesItsCanonicalState(string state, bool visible)
    {
        var d = Parse($$"""{"dictation":{"state":"{{state}}","text":"","can_paste":true} }""").Dictation;
        Assert.Equal(visible, d.ShowPanel);
        Assert.Equal(state == "recording", d.PromptGlow);
        Assert.Equal(state != "refused", d.HasUsableTarget);
    }

    [Fact]
    public void SelectedEngineSectionsMapWithoutSlotProjection()
    {
        var s = Parse("""
            {"tts":{"engine":"system","model":null,"language":null,"provider":null,
                    "status":{"state":"idle","progress":0,"error":null}},
             "stt":{"engine":"claude_code","provider":null,"voice_key":"Space",
                    "status":{"state":"running","progress":0,"error":null}}}
            """);
        Assert.Equal("system", s.TtsEngine.Engine);
        Assert.Equal(EngineState.Idle, s.TtsEngine.Status.State);
        Assert.Equal("claude_code", s.SttEngine.Engine);
        Assert.Equal("Space", s.SttEngine.VoiceKey);
        Assert.Equal(EngineState.Running, s.SttEngine.Status.State);

        var cb = Parse("""
            {"tts":{"engine":"built_in","model":"chatterbox","language":"ru","provider":"cpu",
                    "status":{"state":"running","progress":0,"error":null}}}
            """);
        Assert.Equal("built_in", cb.TtsEngine.Engine);
        Assert.Equal(TtsModel.Chatterbox, cb.TtsEngine.Model);
        Assert.Equal("ru", cb.TtsEngine.Language);
        Assert.Equal(EngineState.Running, cb.TtsEngine.Status.State);

        var unknown = Parse("""
            {"tts":{"engine":"built_in","model":"future_model","language":"en","provider":"cpu",
                    "status":{"state":"running","progress":0,"error":null}}}
            """);
        Assert.Equal("off", unknown.TtsEngine.Engine);
        Assert.Equal(EngineState.Missing, unknown.TtsEngine.Status.State);
    }

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
        var s = Parse($$"""{"tts":{"engine":"built_in","model":"kokoro","status":{"state":"{{state}}","progress":0.5} } }""");
        Assert.Equal(expected, s.TtsEngine.Status.State);
        Assert.Equal(0.5, s.TtsEngine.Status.Progress);
    }

    [Fact]
    public void DiarizationOwnsLifecycleAndDetails()
    {
        var s = Parse("""
            {"diarization":{"status":{"state":"running","progress":1.0,"error":null},
             "enabled":true,"provider":"mlx","speakers":["Alex"],"activity_threshold":0.72}}
            """);
        Assert.Equal(EngineState.Running, s.Diarization.Status.State);
        Assert.True(s.Diarization.Enabled);
        Assert.Equal("mlx", s.Diarization.Runtime);
        Assert.Equal(AlexOnly, s.Diarization.Speakers);
        Assert.Equal(0.72, s.Diarization.ActivityThreshold);
    }

    [Fact]
    public void StatsBlocksMapIntoTheSnapshotGroups()
    {
        var s = Parse("""
            {"stats":{
              "tts":{"rtf_avg":1.2,"rtf_min":1.0,"rtf_max":1.5,"utterances":7,"audio_secs":33.5,"failures":2,"queued":4},
              "stt":{"rtf_avg":0.4,"transcriptions":3,"audio_secs":9.0,"failures":1},
              "lifetime":{"tts_secs":100,"stt_secs":50}}}
            """);
        Assert.Equal(1.2, s.Tts.RtfAvg);
        Assert.Equal(7, s.Tts.Utterances);
        Assert.Equal(2, s.Tts.Failures);
        Assert.Equal(4UL, s.Tts.Queued);
        Assert.Equal(3, s.Stt.Transcriptions);
        Assert.Equal(9.0, s.Stt.AudioSecs);
        Assert.Equal(1, s.Stt.Failures);
        Assert.Equal(100, s.Lifetime.TtsSecs);
        Assert.Equal(50, s.Lifetime.SttSecs);
    }

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
    }
}
