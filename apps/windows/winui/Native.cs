using System;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace DontSpeak;

/// <summary>P/Invoke to ds_core.dll — same C ABI as macOS (dontspeak.h). In-process engine.</summary>
internal static class Native
{
    private const string Dll = "ds_core.dll";

    [DllImport(Dll)] private static extern byte ds_engine_start();
    [DllImport(Dll)] private static extern byte ds_engine_stop();
    [DllImport(Dll)] private static extern IntPtr ds_model_status_json();
    [DllImport(Dll)] private static extern IntPtr ds_model_status_wait(ulong since, uint timeoutMs);
    [DllImport(Dll)] private static extern IntPtr ds_agent_usage_skeleton_json();
    [DllImport(Dll)] private static extern IntPtr ds_agent_usage_card_json([MarshalAs(UnmanagedType.LPUTF8Str)] string agent, byte refresh);
    [DllImport(Dll)] private static extern IntPtr ds_agent_usage_card_authorize_json([MarshalAs(UnmanagedType.LPUTF8Str)] string agent);
    [DllImport(Dll)] private static extern IntPtr ds_tools_json();
    [DllImport(Dll)] private static extern IntPtr ds_libraries_json();
    [DllImport(Dll)] private static extern IntPtr ds_logs_json(uint maxBytes);
    [DllImport(Dll)] private static extern void ds_logs_clear();
    [DllImport(Dll)] private static extern IntPtr ds_version();
    [DllImport(Dll)] private static extern IntPtr ds_homepage_url();
    [DllImport(Dll)] private static extern IntPtr ds_brand_colors_json();
    [DllImport(Dll)] private static extern IntPtr ds_log_colors_json();
    [DllImport(Dll)] private static extern IntPtr ds_random_pastel_wash_json();
    [DllImport(Dll)] private static extern IntPtr ds_update_check_json();
    // Shared status_fmt builders (one impl, all platforms).
    [DllImport(Dll)] private static extern IntPtr ds_engine_state_word([MarshalAs(UnmanagedType.LPUTF8Str)] string state, double progress, [MarshalAs(UnmanagedType.LPUTF8Str)] string why);
    [DllImport(Dll)] private static extern IntPtr ds_duration_live(double secs);
    [DllImport(Dll)] private static extern IntPtr ds_usage_resets_in(long resetsAtUnix);
    [DllImport(Dll)] private static extern IntPtr ds_runtime_label([MarshalAs(UnmanagedType.LPUTF8Str)] string provider);
    [DllImport(Dll)] private static extern IntPtr ds_stats_range(double lo, double avg, double hi, uint precision, [MarshalAs(UnmanagedType.LPUTF8Str)] string unitKey);
    [DllImport(Dll)] private static extern IntPtr ds_stats_count(ulong count, double audioSecs);
    [DllImport(Dll)] private static extern IntPtr ds_human_size(ulong bytes);
    [DllImport(Dll)] private static extern byte ds_diarization_ui_enabled();
    [DllImport(Dll)] private static extern byte ds_agents_ui_enabled();
    [DllImport(Dll)] private static extern IntPtr ds_tray_icon_kind(
        byte sttActive, byte ttsActive, [MarshalAs(UnmanagedType.LPUTF8Str)] string trayIndicatorJson);
    [DllImport(Dll)] private static extern void ds_string_free(IntPtr s);
    [DllImport(Dll)] private static extern byte ds_set_muted(byte on);
    [DllImport(Dll)] private static extern byte ds_open_voice_settings();

    public static bool EngineStart() => ds_engine_start() != 0;
    public static bool EngineStop() => ds_engine_stop() != 0;

    /// <summary>Mute voice output. True if engine accepted the request (not the resulting mute bit).</summary>
    public static bool SetMuted(bool on) => ds_set_muted((byte)(on ? 1 : 0)) != 0;

    /// <summary>Open OS voice settings via shared Rust seam. True if a page launched.</summary>
    public static bool OpenVoiceSettings() => ds_open_voice_settings() != 0;

    public static string EngineStateWord(string state, double progress, string why) => TakeString(ds_engine_state_word(state, progress, why));
    public static string DurationLive(double secs) => TakeString(ds_duration_live(secs));
    public static string UsageResetsIn(long resetsAtUnix) => TakeString(ds_usage_resets_in(resetsAtUnix));
    public static string RuntimeLabel(string provider) => TakeString(ds_runtime_label(provider));
    public static string StatsRange(double lo, double avg, double hi, uint precision, string unitKey) => TakeString(ds_stats_range(lo, avg, hi, precision, unitKey));
    public static string StatsCount(ulong count, double audioSecs) => TakeString(ds_stats_count(count, audioSecs));
    /// <summary>Decimal size string — byte-for-byte parity with macOS/Linux Libraries.</summary>
    public static string HumanSize(ulong bytes) => TakeString(ds_human_size(bytes));

    /// <summary><c>ds_diarization_ui_enabled</c> — single source; do not re-mirror.</summary>
    public static bool DiarizationUiEnabled() => ds_diarization_ui_enabled() != 0;

    /// <summary><c>ds_agents_ui_enabled</c> — initial/engine-down probe; live updates ride model_status.</summary>
    public static bool AgentsUiEnabled() => ds_agents_ui_enabled() != 0;

    /// <summary>
    /// Shared <c>ds_tray_icon_kind</c> — one rule with macOS/Linux. Returns
    /// <c>idle</c> | <c>recording</c> | <c>speaking</c> (unknown/malformed → idle).
    /// </summary>
    public static string TrayIconKind(bool sttActive, bool ttsActive, string[] trayIndicator)
    {
        var json = JsonSerializer.Serialize(trayIndicator ?? Array.Empty<string>());
        return TakeString(ds_tray_icon_kind(
            (byte)(sttActive ? 1 : 0),
            (byte)(ttsActive ? 1 : 0),
            json));
    }

    /// <summary>Map a <c>ds_tray_icon_kind</c> token. Unknown → Idle (shared default).</summary>
    public static TrayIcon.IconState ParseTrayIconKind(string kind) => kind switch
    {
        "recording" => TrayIcon.IconState.Recording,
        "speaking" => TrayIcon.IconState.Speaking,
        _ => TrayIcon.IconState.Idle,
    };

    /// <summary>Cached for process life.</summary>
    public static string Version() => _version ??= TakeString(ds_version());
    private static string? _version;

    public static string HomepageUrl() => TakeString(ds_homepage_url());
    /// <summary>"{}" → brand-hex fallbacks (see Brand).</summary>
    public static string BrandColorsJson() => TakeString(ds_brand_colors_json());
    /// <summary>"{}" → Brand built-in palette.</summary>
    public static string LogColorsJson() => TakeString(ds_log_colors_json());
    /// <summary>One random wash {"r","g","b","a"}; "{}" on failure.</summary>
    public static string RandomPastelWashJson() => TakeString(ds_random_pastel_wash_json());

    /// <summary>BLOCKS on GitHub GET — off UI thread.</summary>
    public static string UpdateCheckJson() => TakeString(ds_update_check_json());

    /// <summary>Missing/malformed ⇒ false (pill only on clear true). Testable without ds_core.dll.</summary>
    internal static bool ParseUpdateAvailable(string json)
    {
        if (string.IsNullOrWhiteSpace(json)) return false;
        try
        {
            var dto = JsonSerializer.Deserialize<UpdateCheckDto>(json, UpdateCheckJsonOptions);
            return dto?.UpdateAvailable ?? false;
        }
        catch { return false; }
    }

    /// <summary>Pill version, or null when ParseUpdateAvailable is false.</summary>
    internal static string? ParseLatestVersion(string json)
    {
        if (!ParseUpdateAvailable(json)) return null;
        try
        {
            var dto = JsonSerializer.Deserialize<UpdateCheckDto>(json, UpdateCheckJsonOptions);
            return string.IsNullOrEmpty(dto?.LatestVersion) ? null : dto.LatestVersion;
        }
        catch { return null; }
    }

    private static readonly JsonSerializerOptions UpdateCheckJsonOptions = new() { PropertyNameCaseInsensitive = true };

    private sealed record UpdateCheckDto(
        [property: JsonPropertyName("update_available")] bool UpdateAvailable,
        [property: JsonPropertyName("latest_version")] string? LatestVersion);

    public static string ModelStatusJson() => TakeString(ds_model_status_json());

    /// <summary>Installed agents + cache; local only.</summary>
    public static string AgentUsageSkeletonJson() => TakeString(ds_agent_usage_skeleton_json());

    /// <summary>BLOCKING single-card load. Off UI; force bypasses 60s soft cache.</summary>
    public static string AgentUsageCardJson(string agent, bool refresh)
        => TakeString(ds_agent_usage_card_json(agent, (byte)(refresh ? 1 : 0)));

    /// <summary>Blocking authorize + force load. Off UI, click only; may ACL-prompt on macOS.</summary>
    public static string AgentUsageAuthorizeCardJson(string agent)
        => TakeString(ds_agent_usage_card_authorize_json(agent));

    /// <summary>BLOCKS until seq ≠ since or timeout. Background only; since=0 first. "{}" if down.</summary>
    public static string ModelStatusWait(ulong since, uint timeoutMs) => TakeString(ds_model_status_wait(since, timeoutMs));

    /// <summary>MCP tool catalog (ds-tools), authored display order.</summary>
    public static string ToolsJson() => TakeString(ds_tools_json());

    /// <summary>Libraries catalog (ds-model) — credits stay lockstep with what ships.</summary>
    public static string LibrariesJson() => TakeString(ds_libraries_json());

    /// <summary>Activity-log tail; "[]" if none.</summary>
    public static string LogsJson(uint maxBytes) => TakeString(ds_logs_json(maxBytes));

    /// <summary>Erase on-disk activity log. Irreversible — confirm first.</summary>
    public static void LogsClear() => ds_logs_clear();

    /// <summary>Marshal Rust UTF-8 char* and free.</summary>
    internal static string TakeString(IntPtr ptr)
    {
        if (ptr == IntPtr.Zero) return "";
        try { return Marshal.PtrToStringUTF8(ptr) ?? ""; }
        finally { ds_string_free(ptr); }
    }
}

public enum EngineState { Missing, Idle, Downloading, Warming, Blocked, Running, Failed }
[JsonConverter(typeof(JsonStringEnumConverter<TtsModel>))]
public enum TtsModel { Kokoro, Chatterbox, Qwen, OmniVoice }

// Word from shared Rust formatter at parse time — one state→word map for all platforms.
public readonly record struct EngineInfo(EngineState State, double Progress, string Word);

/// <summary>Live app activity + tray-tint setting.</summary>
public sealed record Activity
{
    public bool EngineRunning;
    public bool CapsActive, CapsEnabled, Recording, Speaking;
    // Silences voice; playback continues (tray slash + menu checkmark).
    public bool Muted;
    /// Wired client of the in-flight TTS utterance (`claude`/…); null when unattributed.
    public string? Speaker;
    // Tint tokens: stt/tts or stt_animated/tts_animated. Default ["stt","tts_animated"];
    // [] = never tint. Host fallback only — engine is source of truth.
    public string[] TrayIndicator = { "stt", "tts_animated" };
}

public sealed record TtsEngineStatus
{
    public string Engine = "off";
    public TtsModel? Model;
    public string Language = "";
    public string Provider = "";
    public EngineInfo Status;
}

public sealed record SttEngineStatus
{
    public string Engine = "off";
    public string Provider = "";
    public EngineInfo Status;
    public string VoiceKey = "";
}

/// <summary>Dictation confirm-panel wire object.</summary>
public sealed record Dictation
{
    public string DictText = "";
    public bool DictCanPaste = true;
    // Canonical token (ds-status dictation_state.rs).
    public string DictState = "hidden";

    public bool ShowPanel => DictState != "hidden";
    public bool PromptGlow => DictState == "recording" && string.IsNullOrWhiteSpace(DictText);
    public bool HasUsableTarget => DictCanPaste && DictState != "refused";
}

public sealed record TtsStats
{
    public double RtfMin, RtfAvg, RtfMax;
    public double TtfaMinMs, TtfaAvgMs, TtfaMaxMs;
    public double AudioSecs;
    public int Utterances, Failures;
    /// Utterances left to say (waiting + in-flight); live, unlike its cumulative siblings.
    public ulong Queued;
}

public sealed record SttStats
{
    public double RtfMin, RtfAvg, RtfMax, AudioSecs;
    public int Transcriptions, Failures;
}

public sealed record LifetimeStats
{
    public double TtsSecs, SttSecs;
}

public sealed record DiarizationStatus
{
    public EngineInfo Status;
    public bool Enabled;
    public string Runtime = "";
    public string[] Speakers = Array.Empty<string>();
    public double ActivityThreshold;
}

/// <summary>Parsed model-status snapshot (macOS HealthSnapshot parity).</summary>
internal sealed class HealthSnapshot
{
    public Activity Activity = new();
    public TtsEngineStatus TtsEngine = new();
    public SttEngineStatus SttEngine = new();
    public DiarizationStatus Diarization = new();
    public Dictation Dictation = new();

    /// <summary>Shared <c>ds_tray_icon_kind</c> — one rule with macOS/Linux (not re-mirrored in C#).</summary>
    public TrayIcon.IconState IndicatorState() =>
        Native.ParseTrayIconKind(
            Native.TrayIconKind(Activity.Recording, Activity.Speaking, Activity.TrayIndicator));
    public TtsStats Tts = new();
    public SttStats Stt = new();
    public LifetimeStats Lifetime = new();

    // Echo as `since` to ModelStatusWait to block until next change.
    public ulong StatusSeq;

    // Config `agents` gate; null when the snapshot is undecodable / engine down
    // (host keeps last known instead of hiding the tab on a blip).
    public bool? AgentsEnabled;

    public static HealthSnapshot Probe() => FromJson(Native.ModelStatusJson());

    // Case-insensitive property names; a grown schema may add object members.
    private static readonly JsonSerializerOptions ModelStatusJsonOptions = new()
    {
        PropertyNameCaseInsensitive = true,
    };

    /// <summary>Parse model-status JSON. Push path already holds JSON — reuse, don't re-fetch.</summary>
    public static HealthSnapshot FromJson(string json) => FromJson(json, Native.EngineStateWord);

    /// <summary>Injectable state-word formatter (tests without ds_core.dll).</summary>
    public static HealthSnapshot FromJson(string json, Func<string, double, string, string> stateWord)
    {
        var s = new HealthSnapshot();
        if (string.IsNullOrWhiteSpace(json) || json == "{}") return s;
        try
        {
            var dto = JsonSerializer.Deserialize<ModelStatusDto>(json, ModelStatusJsonOptions);
            if (dto is null) return s;
            // Well-formed non-empty JSON ⇒ engine up; malformed → catch → empty.
            s.Activity.EngineRunning = true;
            s.StatusSeq = dto.Seq;
            s.AgentsEnabled = dto.Agents;

            if (dto.Activity is { } activity)
            {
                s.Activity.CapsActive = activity.CapsActive;
                s.Activity.CapsEnabled = activity.CapsEnabled;
                s.Activity.Recording = activity.Recording;
                s.Activity.Speaking = activity.Speaking;
                s.Activity.Speaker = activity.Speaking ? activity.Speaker : null;
                s.Activity.Muted = activity.Muted;
            }
            // Keep host default when key absent.
            if (dto.TrayIndicator is { } ti)
                s.Activity.TrayIndicator = ti.Where(t => t is not null).Cast<string>().ToArray();
            if (dto.Dictation is { } d)
            {
                s.Dictation.DictText = d.Text ?? "";
                s.Dictation.DictCanPaste = d.CanPaste;
                s.Dictation.DictState = d.State ?? "hidden";
            }
            if (dto.Tts is { } ttsStatus)
            {
                s.TtsEngine.Engine = ttsStatus.Engine ?? "off";
                s.TtsEngine.Model = ttsStatus.Model;
                s.TtsEngine.Language = ttsStatus.Language ?? "";
                s.TtsEngine.Provider = ttsStatus.Provider ?? "";
                s.TtsEngine.Status = ToEngine(ttsStatus.Status, stateWord);
            }
            if (dto.Stt is { } sttStatus)
            {
                s.SttEngine.Engine = sttStatus.Engine ?? "off";
                s.SttEngine.Provider = sttStatus.Provider ?? "";
                s.SttEngine.VoiceKey = sttStatus.VoiceKey ?? "";
                s.SttEngine.Status = ToEngine(sttStatus.Status, stateWord);
            }
            if (dto.Diarization is { } diarization)
            {
                s.Diarization.Status = ToEngine(diarization.Status, stateWord);
                s.Diarization.Enabled = diarization.Enabled;
                s.Diarization.Runtime = diarization.Provider ?? "";
                s.Diarization.Speakers = diarization.Speakers?.Where(x => x is not null).Cast<string>().ToArray() ?? Array.Empty<string>();
                s.Diarization.ActivityThreshold = diarization.ActivityThreshold;
            }
            if (dto.Stats is { } stats)
            {
                if (stats.Tts is { } tts)
                {
                    s.Tts.RtfMin = tts.RtfMin; s.Tts.RtfAvg = tts.RtfAvg; s.Tts.RtfMax = tts.RtfMax;
                    s.Tts.TtfaMinMs = tts.TtfaMinMs; s.Tts.TtfaAvgMs = tts.TtfaAvgMs; s.Tts.TtfaMaxMs = tts.TtfaMaxMs;
                    s.Tts.Utterances = (int)tts.Utterances; s.Tts.AudioSecs = tts.AudioSecs; s.Tts.Failures = (int)tts.Failures;
                    s.Tts.Queued = tts.Queued;
                }
                if (stats.Stt is { } stt)
                {
                    s.Stt.RtfMin = stt.RtfMin; s.Stt.RtfAvg = stt.RtfAvg; s.Stt.RtfMax = stt.RtfMax;
                    s.Stt.Transcriptions = (int)stt.Transcriptions; s.Stt.AudioSecs = stt.AudioSecs;
                    s.Stt.Failures = (int)stt.Failures;
                }
                if (stats.Lifetime is { } lt)
                {
                    s.Lifetime.TtsSecs = lt.TtsSecs;
                    s.Lifetime.SttSecs = lt.SttSecs;
                }
            }
        }
        catch { /* mid-write / malformed → empty */ }
        return s;
    }

    /// <summary>`state` string → enum 1:1 with dontspeakd; missing object → Missing; word from shared Rust.</summary>
    private static EngineInfo ToEngine(EngineStatusDto? o, Func<string, double, string, string> stateWord)
    {
        if (o is null)
            return new EngineInfo(EngineState.Missing, 0, stateWord("missing", 0, ""));
        var state = o.State ?? "";
        var pct = o.Progress;
        var why = o.Error ?? "";
        var es = state switch
        {
            "running" => EngineState.Running,
            "idle" => EngineState.Idle,
            "warming" => EngineState.Warming,
            "blocked" => EngineState.Blocked,
            "failed" => EngineState.Failed,
            "downloading" => EngineState.Downloading,
            _ => EngineState.Missing,
        };
        return new EngineInfo(es, pct, stateWord(state, pct, why));
    }

}

// Hand mirror of ds-status model_status JSON. No codegen — round-trip test keeps lockstep.
// Unknown members OK.

internal sealed record ModelStatusDto
{
    [JsonPropertyName("seq")] public ulong Seq { get; init; }
    [JsonPropertyName("activity")] public ActivityDto? Activity { get; init; }
    [JsonPropertyName("tts")] public TtsStatusDto? Tts { get; init; }
    [JsonPropertyName("stt")] public SttStatusDto? Stt { get; init; }
    [JsonPropertyName("diarization")] public DiarizationStatusDto? Diarization { get; init; }
    [JsonPropertyName("dictation")] public DictationDto? Dictation { get; init; }
    [JsonPropertyName("stats")] public StatsDto? Stats { get; init; }
    [JsonPropertyName("tray")] public string?[]? TrayIndicator { get; init; }
    [JsonPropertyName("downloads")] public DownloadStatusDto[]? Downloads { get; init; }
    [JsonPropertyName("agents")] public bool Agents { get; init; }
}

internal sealed record EngineStatusDto
{
    [JsonPropertyName("state")] public string? State { get; init; }
    [JsonPropertyName("progress")] public double Progress { get; init; }
    [JsonPropertyName("error")] public string? Error { get; init; }
}

internal sealed record ActivityDto
{
    [JsonPropertyName("caps")] public bool CapsEnabled { get; init; }
    [JsonPropertyName("caps_active")] public bool CapsActive { get; init; }
    [JsonPropertyName("recording")] public bool Recording { get; init; }
    [JsonPropertyName("speaking")] public bool Speaking { get; init; }
    [JsonPropertyName("speaker")] public string? Speaker { get; init; }
    [JsonPropertyName("utterance_id")] public ulong? UtteranceId { get; init; }
    [JsonPropertyName("voice")] public string? Voice { get; init; }
    [JsonPropertyName("language")] public string? Language { get; init; }
    [JsonPropertyName("warning")] public string? Warning { get; init; }
    [JsonPropertyName("muted")] public bool Muted { get; init; }
}

internal sealed record UtteranceStatusDto
{
    [JsonPropertyName("id")] public ulong Id { get; init; }
    [JsonPropertyName("voice")] public string? Voice { get; init; }
    [JsonPropertyName("language")] public string? Language { get; init; }
    [JsonPropertyName("warning")] public string? Warning { get; init; }
    [JsonPropertyName("outcome")] public string? Outcome { get; init; }
}

internal sealed record DownloadStatusDto
{
    [JsonPropertyName("target")] public string? Target { get; init; }
    [JsonPropertyName("done_bytes")] public ulong DoneBytes { get; init; }
    [JsonPropertyName("total_bytes")] public ulong TotalBytes { get; init; }
    [JsonPropertyName("start_bytes")] public ulong StartBytes { get; init; }
    [JsonPropertyName("elapsed_seconds")] public ulong ElapsedSeconds { get; init; }
}

internal sealed record TtsStatusDto
{
    [JsonPropertyName("engine")] public string? Engine { get; init; }
    [JsonPropertyName("model")] public required TtsModel? Model { get; init; }
    [JsonPropertyName("language")] public string? Language { get; init; }
    [JsonPropertyName("provider")] public string? Provider { get; init; }
    [JsonPropertyName("status")] public EngineStatusDto? Status { get; init; }
    [JsonPropertyName("recent_utterances")] public UtteranceStatusDto[]? RecentUtterances { get; init; }
}

internal sealed record SttStatusDto
{
    [JsonPropertyName("engine")] public string? Engine { get; init; }
    [JsonPropertyName("provider")] public string? Provider { get; init; }
    [JsonPropertyName("status")] public EngineStatusDto? Status { get; init; }
    [JsonPropertyName("voice_key")] public string? VoiceKey { get; init; }
}

internal sealed record DictationDto
{
    [JsonPropertyName("state")] public string? State { get; init; }
    [JsonPropertyName("text")] public string? Text { get; init; }
    [JsonPropertyName("can_paste")] public bool CanPaste { get; init; }
}

internal sealed record StatsDto
{
    [JsonPropertyName("tts")] public TtsStatsDto? Tts { get; init; }
    [JsonPropertyName("stt")] public SttStatsDto? Stt { get; init; }
    [JsonPropertyName("lifetime")] public LifetimeDto? Lifetime { get; init; }
}

internal sealed record TtsStatsDto
{
    [JsonPropertyName("rtf_min")] public double RtfMin { get; init; }
    [JsonPropertyName("rtf_avg")] public double RtfAvg { get; init; }
    [JsonPropertyName("rtf_max")] public double RtfMax { get; init; }
    [JsonPropertyName("ttfa_min_ms")] public double TtfaMinMs { get; init; }
    [JsonPropertyName("ttfa_avg_ms")] public double TtfaAvgMs { get; init; }
    [JsonPropertyName("ttfa_max_ms")] public double TtfaMaxMs { get; init; }
    [JsonPropertyName("utterances")] public long Utterances { get; init; }
    [JsonPropertyName("audio_secs")] public double AudioSecs { get; init; }
    [JsonPropertyName("failures")] public long Failures { get; init; }
    [JsonPropertyName("queued")] public ulong Queued { get; init; }
}

internal sealed record SttStatsDto
{
    [JsonPropertyName("rtf_min")] public double RtfMin { get; init; }
    [JsonPropertyName("rtf_avg")] public double RtfAvg { get; init; }
    [JsonPropertyName("rtf_max")] public double RtfMax { get; init; }
    [JsonPropertyName("transcriptions")] public long Transcriptions { get; init; }
    [JsonPropertyName("audio_secs")] public double AudioSecs { get; init; }
    [JsonPropertyName("failures")] public long Failures { get; init; }
}

internal sealed record LifetimeDto
{
    [JsonPropertyName("tts_secs")] public long TtsSecs { get; init; }
    [JsonPropertyName("stt_secs")] public long SttSecs { get; init; }
}

internal sealed record DiarizationStatusDto
{
    [JsonPropertyName("status")] public EngineStatusDto? Status { get; init; }
    [JsonPropertyName("enabled")] public bool Enabled { get; init; }
    [JsonPropertyName("provider")] public string? Provider { get; init; }
    [JsonPropertyName("speakers")] public string?[]? Speakers { get; init; }
    [JsonPropertyName("activity_threshold")] public double ActivityThreshold { get; init; }
}
