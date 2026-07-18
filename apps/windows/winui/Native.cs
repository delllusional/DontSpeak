using System;
using System.Collections.Generic;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace DontSpeak;

/// <summary>
/// P/Invoke to <c>ds_core.dll</c> — same C ABI as macOS (<c>dontspeak.h</c>). Hosts the engine
/// in-process: <see cref="EngineStart"/> / <see cref="EngineStop"/> for caps loop + RPC + TTS.
/// </summary>
internal static class Native
{
    private const string Dll = "ds_core.dll";

    [DllImport(Dll)] private static extern byte ds_engine_start();
    [DllImport(Dll)] private static extern byte ds_engine_stop();
    [DllImport(Dll)] private static extern IntPtr ds_model_status_json();
    [DllImport(Dll)] private static extern IntPtr ds_model_status_wait(ulong since, uint timeoutMs);
    [DllImport(Dll)] private static extern IntPtr ds_agent_usage_skeleton_json();
    [DllImport(Dll)] private static extern IntPtr ds_agent_usage_card_json([MarshalAs(UnmanagedType.LPUTF8Str)] string agent, byte forceRefresh);
    [DllImport(Dll)] private static extern IntPtr ds_tools_json();
    [DllImport(Dll)] private static extern IntPtr ds_libraries_json();
    [DllImport(Dll)] private static extern IntPtr ds_logs_json(uint maxBytes);
    [DllImport(Dll)] private static extern IntPtr ds_logs_wait(uint maxBytes, uint timeoutMs);
    [DllImport(Dll)] private static extern void ds_logs_clear();
    [DllImport(Dll)] private static extern IntPtr ds_version();
    [DllImport(Dll)] private static extern IntPtr ds_homepage_url();
    [DllImport(Dll)] private static extern IntPtr ds_brand_colors_json();
    [DllImport(Dll)] private static extern IntPtr ds_log_colors_json();
    [DllImport(Dll)] private static extern IntPtr ds_update_check_json();
    // Shared status-panel formatters (one impl, every platform UI).
    [DllImport(Dll)] private static extern IntPtr ds_engine_state_word([MarshalAs(UnmanagedType.LPUTF8Str)] string state, double progress, [MarshalAs(UnmanagedType.LPUTF8Str)] string why);
    [DllImport(Dll)] private static extern IntPtr ds_duration_live(double secs);
    [DllImport(Dll)] private static extern IntPtr ds_usage_resets_in(long resetsAtUnix);
    [DllImport(Dll)] private static extern IntPtr ds_runtime_label([MarshalAs(UnmanagedType.LPUTF8Str)] string provider);
    [DllImport(Dll)] private static extern IntPtr ds_stats_range(double lo, double avg, double hi, uint precision, [MarshalAs(UnmanagedType.LPUTF8Str)] string unitKey);
    [DllImport(Dll)] private static extern IntPtr ds_stats_count(ulong count, double audioSecs);
    [DllImport(Dll)] private static extern IntPtr ds_human_size(ulong bytes);
    [DllImport(Dll)] private static extern IntPtr ds_tray_icon_kind(byte sttActive, byte ttsActive, [MarshalAs(UnmanagedType.LPUTF8Str)] string trayIndicatorJson);
    [DllImport(Dll)] private static extern IntPtr ds_active_tts_slot([MarshalAs(UnmanagedType.LPUTF8Str)] string ttsEngine);
    [DllImport(Dll)] private static extern IntPtr ds_active_stt_slot([MarshalAs(UnmanagedType.LPUTF8Str)] string sttEngine);
    [DllImport(Dll)] private static extern byte ds_diarization_ui_enabled();
    [DllImport(Dll)] private static extern void ds_string_free(IntPtr s);
    [DllImport(Dll)] private static extern byte ds_set_muted(byte on);
    [DllImport(Dll)] private static extern byte ds_open_voice_settings();

    public static bool EngineStart() => ds_engine_start() != 0;
    public static bool EngineStop() => ds_engine_stop() != 0;

    /// <summary>Silence playback without stopping it. Returns true if the request reached the
    /// engine (false = engine down), NOT the resulting muted state.</summary>
    public static bool SetMuted(bool on) => ds_set_muted((byte)(on ? 1 : 0)) != 0;

    /// <summary>Open OS voice settings (Windows: Time &amp; language ▸ Speech) via the shared
    /// Rust seam. Returns true if a page was launched.</summary>
    public static bool OpenVoiceSettings() => ds_open_voice_settings() != 0;

    public static string EngineStateWord(string state, double progress, string why) => TakeString(ds_engine_state_word(state, progress, why));
    public static string DurationLive(double secs) => TakeString(ds_duration_live(secs));
    public static string UsageResetsIn(long resetsAtUnix) => TakeString(ds_usage_resets_in(resetsAtUnix));
    public static string RuntimeLabel(string provider) => TakeString(ds_runtime_label(provider));
    public static string StatsRange(double lo, double avg, double hi, uint precision, string unitKey) => TakeString(ds_stats_range(lo, avg, hi, precision, unitKey));
    public static string StatsCount(ulong count, double audioSecs) => TakeString(ds_stats_count(count, audioSecs));
    /// <summary>Decimal size string shared with macOS/Linux Libraries tabs (byte-for-byte parity).</summary>
    public static string HumanSize(ulong bytes) => TakeString(ds_human_size(bytes));

    /// <summary><c>ds_tray_icon_kind</c>.</summary>
    public static string TrayIconKind(bool sttActive, bool ttsActive, string[] trayIndicator)
    {
        var json = JsonSerializer.Serialize(trayIndicator ?? Array.Empty<string>());
        return TakeString(ds_tray_icon_kind((byte)(sttActive ? 1 : 0), (byte)(ttsActive ? 1 : 0), json));
    }

    /// <summary><c>ds_active_tts_slot</c>.</summary>
    public static string ActiveTtsSlot(string ttsEngine) => TakeString(ds_active_tts_slot(ttsEngine ?? ""));

    /// <summary><c>ds_active_stt_slot</c>.</summary>
    public static string ActiveSttSlot(string sttEngine) => TakeString(ds_active_stt_slot(sttEngine ?? ""));

    /// <summary><c>ds_diarization_ui_enabled</c> — do not re-mirror.</summary>
    public static bool DiarizationUiEnabled() => ds_diarization_ui_enabled() != 0;

    /// <summary>Workspace product version; cached (immutable for process life; ApplyStatus reads often).</summary>
    public static string Version() => _version ??= TakeString(ds_version());
    private static string? _version;

    public static string HomepageUrl() => TakeString(ds_homepage_url());
    /// <summary>Brand tints JSON; "{}" → callers use brand-hex fallbacks (see <see cref="Brand"/>).</summary>
    public static string BrandColorsJson() => TakeString(ds_brand_colors_json());
    /// <summary>Logs-tab colors JSON; "{}" → <see cref="Brand"/> built-in palette.</summary>
    public static string LogColorsJson() => TakeString(ds_log_colors_json());

    /// <summary>BLOCKS on GitHub API GET — call off UI thread. Parse with
    /// <see cref="ParseUpdateAvailable"/> / <see cref="ParseLatestVersion"/>.</summary>
    public static string UpdateCheckJson() => TakeString(ds_update_check_json());

    /// <summary>Pure parse: missing/malformed ⇒ false (never show the pill on ambiguity).
    /// Internal for unit tests without ds_core.dll.</summary>
    internal static bool ParseUpdateAvailable(string json)
    {
        if (string.IsNullOrWhiteSpace(json)) return false;
        try
        {
            var dto = JsonSerializer.Deserialize<UpdateCheckDto>(json, UpdateCheckJsonOptions);
            return dto?.UpdateAvailable ?? false;
        }
        catch { return false; } // malformed → never show pill on ambiguity
    }

    /// <summary>Pill version string, or null whenever <see cref="ParseUpdateAvailable"/> is false.</summary>
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

    /// <summary>Instant deck: installed agent cards + cache. No network.</summary>
    public static string AgentUsageSkeletonJson() => TakeString(ds_agent_usage_skeleton_json());

    /// <summary>BLOCKING single-card load. Off UI thread; force bypasses 60s soft cache.</summary>
    public static string AgentUsageCardJson(string agent, bool forceRefresh)
        => TakeString(ds_agent_usage_card_json(agent, (byte)(forceRefresh ? 1 : 0)));

    /// <summary>BLOCKS until status seq ≠ <paramref name="since"/> or timeout; returns model-status
    /// JSON ("seq" is next since). Dedicated background thread only; since=0 first. "{}" if down.</summary>
    public static string ModelStatusWait(ulong since, uint timeoutMs) => TakeString(ds_model_status_wait(since, timeoutMs));

    /// <summary>MCP tool catalog (ds-tools), authored display order — same as macOS ToolsView.</summary>
    public static string ToolsJson() => TakeString(ds_tools_json());

    /// <summary>Libraries catalog (ds-model); shared so credits can't drift from what ships.</summary>
    public static string LibrariesJson() => TakeString(ds_libraries_json());

    /// <summary>Combined activity-log tail ({source, level, text}); unified + aux logs. "[]" if none.</summary>
    public static string LogsJson(uint maxBytes) => TakeString(ds_logs_json(maxBytes));

    /// <summary>Like <see cref="LogsJson"/> but BLOCKS on log-dir change (client-side fs watch, not
    /// engine RPC; rotated *.log.N ignored) or timeout. Dedicated background thread only.</summary>
    public static string LogsWait(uint maxBytes, uint timeoutMs) => TakeString(ds_logs_wait(maxBytes, timeoutMs));

    /// <summary>Erase on-disk activity log (unified + rotated + aux). Irreversible — confirm first.</summary>
    public static void LogsClear() => ds_logs_clear();

    /// <summary>Marshal Rust UTF-8 char* and free. Shared by <see cref="Loc"/> and this class.</summary>
    internal static string TakeString(IntPtr ptr)
    {
        if (ptr == IntPtr.Zero) return "";
        try { return Marshal.PtrToStringUTF8(ptr) ?? ""; }
        finally { ds_string_free(ptr); }
    }
}

public enum EngineState { Missing, Idle, Downloading, Warming, Blocked, Running, Failed }

// Word from shared Rust formatter at parse time — one state→word map for all platforms.
public readonly record struct EngineInfo(EngineState State, double Progress, string Word);

/// <summary>Engine `running` map + tray-tint setting.</summary>
public sealed record Activity
{
    public bool EngineRunning;
    public bool Caps, Recording, Speaking;
    // Mute silences voice without stopping playback (tray slash + menu checkmark).
    public bool Muted;
    /// Wireable client of the in-flight TTS utterance (`claude_code`/…); null when idle.
    public string? TtsSource;
    // Tray tint tokens: stt/tts or stt_animated/tts_animated. Default ["stt","tts_animated"];
    // [] = never tint. Fallback only — engine is source of truth.
    public string[] TrayIndicator = { "stt", "tts_animated" };
}

/// <summary>Lifecycle dots for every engine object on the wire (local, OS, delegate, diarization).</summary>
public sealed record EngineDots
{
    public EngineInfo Kokoro, Parakeet, ClaudeCode, System, TtsSystem, Diarization;
}

/// <summary>Active STT/TTS tokens + runtime EPs (drives which row/dot/stats render).</summary>
public sealed record EngineSelection
{
    // stt: claude_code | built_in | system; tts: built_in | system
    public string SttEngine = "claude_code", TtsEngine = "built_in";
    // built_in runtime EP (cpu|cuda|coreml|ane); empty = none
    public string SttProvider = "", TtsProvider = "";
    // Claude Code synthesized key label (e.g. "Space"); empty if N/A
    public string ClaudeCodeKey = "";
}

/// <summary>Dictation confirm-panel wire object (transcript + panel gating fields).</summary>
public sealed record Dictation
{
    public string DictText = "";
    public bool DictAwaitingConfirm, DictLocalStt;
    // Editable paste target focused? Fail-open true (old engines omit the key).
    public bool DictHasTarget = true;
    // Core-computed "speak now" glow — shared with macOS; no-target uses DictHasTarget.
    public bool DictPromptGlow;
    // Start refused (model missing/downloading/warming). Fail-quiet false. Warning glow like no-target.
    public bool DictRefused;
    // Canonical token (ds-status dictation_state.rs). Empty ⇒ older engine ⇒ legacy boolean fallback.
    public string DictState = "";

    /// <summary>Visibility from `dictation.state`; unknown/absent falls back to legacy booleans.</summary>
    public bool ShowPanel(bool recording) => DictState switch
    {
        "hidden" => false,
        "recording" or "awaiting_confirm" or "refused" => true,
        _ => DictAwaitingConfirm || (recording && DictLocalStt) || DictRefused,
    };
}

public sealed record TtsStats
{
    public double RtfAvg, RtfMin, RtfMax;
    public double FirstAvgMs, FirstMinMs, FirstMaxMs;
    public double AudioSecs;
    public int Utterances, Failures;
}

public sealed record SttStats
{
    public double RtfAvg, RtfMin, RtfMax, AudioSecs;
    public int Transcriptions, Failures;
}

public sealed record LifetimeStats
{
    public double TtsSecs, SttSecs;
}

/// <summary>Diarization UI fields only (`present`/`speaker_threshold` on wire but unused, macOS parity).</summary>
public sealed record DiarizationStats
{
    public bool Enabled;
    public string Runtime = "";
    public string[] Speakers = Array.Empty<string>();
    public double ClusteringThreshold;
}

/// <summary>Parsed model-status snapshot (macOS HealthSnapshot parity).</summary>
internal sealed class HealthSnapshot
{
    public Activity Activity = new();
    public EngineDots EngineDots = new();
    public EngineSelection EngineSelection = new();
    public Dictation Dictation = new();

    /// <summary>See <c>ds_status::ActiveTtsSlot</c> (pure; no ds_core.dll in tests).</summary>
    public EngineInfo ActiveTts => EngineSelection.TtsEngine switch
    {
        "system" => EngineDots.TtsSystem,
        "built_in" => EngineDots.Kokoro,
        _ => new EngineInfo(EngineState.Missing, 0, ""), // off / unknown
    };
    /// <summary>See <c>ds_status::ActiveSttSlot</c>.</summary>
    public EngineInfo ActiveStt => EngineSelection.SttEngine switch
    {
        "claude_code" => EngineDots.ClaudeCode,
        "system" => EngineDots.System,
        "built_in" => EngineDots.Parakeet,
        _ => new EngineInfo(EngineState.Missing, 0, ""),
    };

    /// <summary>See <c>ds_status::tray_icon_kind</c>.</summary>
    public TrayIcon.IconState IndicatorState()
    {
        bool Colors(string state) =>
            Array.IndexOf(Activity.TrayIndicator, state) >= 0 ||
            Array.IndexOf(Activity.TrayIndicator, state + "_animated") >= 0;
        if (Activity.Recording && Colors("stt")) return TrayIcon.IconState.Recording;
        if (Activity.Speaking && Colors("tts")) return TrayIcon.IconState.Speaking;
        return TrayIcon.IconState.Idle;
    }
    public TtsStats Tts = new();
    public SttStats Stt = new();
    public LifetimeStats Lifetime = new();
    public DiarizationStats Diarization = new();

    // status.rs StatusGate: echo as `since` to ModelStatusWait to block until next change.
    public ulong StatusSeq;

    public static HealthSnapshot Probe() => FromJson(Native.ModelStatusJson());

    // Case-insensitive + tolerant of unknown members so a grown Rust schema never throws here.
    private static readonly JsonSerializerOptions ModelStatusJsonOptions = new()
    {
        PropertyNameCaseInsensitive = true,
    };

    /// <summary>Parse model-status JSON. Push path already holds JSON — must not re-fetch.</summary>
    public static HealthSnapshot FromJson(string json) => FromJson(json, Native.EngineStateWord);

    /// <summary>As <see cref="FromJson(string)"/> with injectable state-word formatter
    /// (tests stub so parse runs without ds_core.dll).</summary>
    public static HealthSnapshot FromJson(string json, Func<string, double, string, string> stateWord)
    {
        var s = new HealthSnapshot();
        if (string.IsNullOrWhiteSpace(json) || json == "{}") return s;
        try
        {
            var dto = JsonSerializer.Deserialize<ModelStatusDto>(json, ModelStatusJsonOptions);
            if (dto is null) return s;
            // Well-formed non-empty JSON ⇒ engine up; malformed → catch → empty snapshot.
            s.Activity.EngineRunning = true;
            s.StatusSeq = dto.Seq;

            if (dto.Running is { } r)
            {
                s.Activity.Caps = r.Caps;
                s.Activity.Recording = r.SttActive;
                s.Activity.Speaking = r.TtsActive;
                s.Activity.TtsSource = r.TtsActive ? r.TtsSource : null;
                s.Activity.Muted = r.Muted;
            }
            // Override default tray_indicator only when key present (null ⇒ keep default).
            if (dto.TrayIndicator is { } ti)
                s.Activity.TrayIndicator = ti.Where(t => t is not null).Cast<string>().ToArray();
            if (dto.Dictation is { } d)
            {
                s.Dictation.DictText = d.Text ?? "";
                s.Dictation.DictAwaitingConfirm = d.AwaitingConfirm;
                s.Dictation.DictLocalStt = d.LocalStt;
                // Fail-open: true unless engine explicitly says false.
                s.Dictation.DictHasTarget = d.HasPasteTarget ?? true;
                s.Dictation.DictPromptGlow = d.PromptGlow;
                s.Dictation.DictRefused = d.Refused;
                s.Dictation.DictState = d.State ?? "";
            }
            // Partial payload still picks a row via engine defaults.
            s.EngineSelection.SttEngine = NonEmptyOr(dto.SttEngine, "claude_code");
            s.EngineSelection.TtsEngine = NonEmptyOr(dto.TtsEngine, "built_in");
            s.EngineSelection.SttProvider = dto.SttProvider ?? "";
            s.EngineSelection.TtsProvider = dto.TtsProvider ?? "";
            s.EngineSelection.ClaudeCodeKey = dto.ClaudeCodeKey ?? "";
            s.EngineDots.Kokoro = ToEngine(dto.Kokoro, stateWord);
            s.EngineDots.Parakeet = ToEngine(dto.Parakeet, stateWord);
            s.EngineDots.ClaudeCode = ToEngine(dto.ClaudeCode, stateWord);
            s.EngineDots.System = ToEngine(dto.System, stateWord);
            s.EngineDots.TtsSystem = ToEngine(dto.TtsSystem, stateWord);
            s.EngineDots.Diarization = ToEngine(dto.Diarization, stateWord);
            if (dto.Stats is { } stats)
            {
                if (stats.Tts is { } tts)
                {
                    s.Tts.RtfAvg = tts.RtfAvg; s.Tts.RtfMin = tts.RtfMin; s.Tts.RtfMax = tts.RtfMax;
                    s.Tts.FirstAvgMs = tts.FirstAvgMs; s.Tts.FirstMinMs = tts.FirstMinMs; s.Tts.FirstMaxMs = tts.FirstMaxMs;
                    s.Tts.Utterances = (int)tts.Utterances; s.Tts.AudioSecs = tts.AudioSecs; s.Tts.Failures = (int)tts.Failures;
                }
                if (stats.Stt is { } stt)
                {
                    s.Stt.RtfAvg = stt.RtfAvg; s.Stt.RtfMin = stt.RtfMin; s.Stt.RtfMax = stt.RtfMax;
                    s.Stt.Transcriptions = (int)stt.Transcriptions; s.Stt.AudioSecs = stt.AudioSecs;
                    s.Stt.Failures = (int)stt.Failures;
                }
                if (stats.Lifetime is { } lt)
                {
                    s.Lifetime.TtsSecs = lt.TtsSecs;
                    s.Lifetime.SttSecs = lt.SttSecs;
                }
                if (stats.Diarization is { } diar)
                {
                    s.Diarization.Enabled = diar.Enabled;
                    s.Diarization.Runtime = diar.Runtime ?? "";
                    s.Diarization.Speakers = diar.Speakers?.Where(x => x is not null).Cast<string>().ToArray() ?? Array.Empty<string>();
                    s.Diarization.ClusteringThreshold = diar.ClusteringThreshold;
                }
            }
        }
        catch { /* mid-write / malformed → empty snapshot */ }
        return s;
    }

    private static string NonEmptyOr(string? v, string fallback) =>
        string.IsNullOrEmpty(v) ? fallback : v;

    /// <summary>`state` string → enum 1:1 with dontspeakd; missing object → Missing; word from shared Rust.</summary>
    private static EngineInfo ToEngine(EngineObjDto? o, Func<string, double, string, string> stateWord)
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

// Hand mirror of ds-status model_status JSON (dontspeakd/src/status.rs). No codegen — kept honest
// by ds-status round-trip test. Lockstep with Rust; [JsonPropertyName] only; unknown members OK.
// Feeds HealthSnapshot.FromJson only.

internal sealed record ModelStatusDto
{
    [JsonPropertyName("kokoro")] public EngineObjDto? Kokoro { get; init; }
    [JsonPropertyName("parakeet")] public EngineObjDto? Parakeet { get; init; }
    [JsonPropertyName("diarization")] public EngineObjDto? Diarization { get; init; }
    [JsonPropertyName("system")] public EngineObjDto? System { get; init; }
    [JsonPropertyName("claude_code")] public EngineObjDto? ClaudeCode { get; init; }
    [JsonPropertyName("tts_system")] public EngineObjDto? TtsSystem { get; init; }

    [JsonPropertyName("stt_engine")] public string? SttEngine { get; init; }
    [JsonPropertyName("stt_provider")] public string? SttProvider { get; init; }
    [JsonPropertyName("tts_engine")] public string? TtsEngine { get; init; }
    [JsonPropertyName("tts_provider")] public string? TtsProvider { get; init; }
    [JsonPropertyName("claude_code_key")] public string? ClaudeCodeKey { get; init; }

    [JsonPropertyName("running")] public RunningDto? Running { get; init; }
    [JsonPropertyName("dictation")] public DictationDto? Dictation { get; init; }
    [JsonPropertyName("tray_indicator")] public string?[]? TrayIndicator { get; init; }
    [JsonPropertyName("stats")] public StatsDto? Stats { get; init; }
    [JsonPropertyName("caps_events")] public CapsEventDto[]? CapsEvents { get; init; }
    [JsonPropertyName("build_id")] public string? BuildId { get; init; }
    [JsonPropertyName("seq")] public ulong Seq { get; init; }
}

internal sealed record EngineObjDto
{
    [JsonPropertyName("present")] public bool Present { get; init; }
    [JsonPropertyName("removable")] public bool Removable { get; init; }
    [JsonPropertyName("state")] public string? State { get; init; }
    [JsonPropertyName("progress")] public double Progress { get; init; }
    [JsonPropertyName("error")] public string? Error { get; init; }
}

internal sealed record RunningDto
{
    [JsonPropertyName("caps")] public bool Caps { get; init; }
    [JsonPropertyName("caps_wanted")] public bool CapsWanted { get; init; }
    [JsonPropertyName("stt_active")] public bool SttActive { get; init; }
    [JsonPropertyName("tts_active")] public bool TtsActive { get; init; }
    [JsonPropertyName("tts_source")] public string? TtsSource { get; init; }
    [JsonPropertyName("muted")] public bool Muted { get; init; }
    [JsonPropertyName("kokoro")] public bool Kokoro { get; init; }
    [JsonPropertyName("tts_system")] public bool TtsSystem { get; init; }
    [JsonPropertyName("parakeet")] public bool Parakeet { get; init; }
    [JsonPropertyName("system")] public bool System { get; init; }
    [JsonPropertyName("claude_code")] public bool ClaudeCode { get; init; }
}

internal sealed record DictationDto
{
    [JsonPropertyName("recording")] public bool Recording { get; init; }
    [JsonPropertyName("awaiting_confirm")] public bool AwaitingConfirm { get; init; }
    [JsonPropertyName("text")] public string? Text { get; init; }
    [JsonPropertyName("target")] public string? Target { get; init; }
    [JsonPropertyName("local_stt")] public bool LocalStt { get; init; }
    // Nullable: absent ⇒ fail-open true vs explicit false.
    [JsonPropertyName("has_paste_target")] public bool? HasPasteTarget { get; init; }
    [JsonPropertyName("prompt_glow")] public bool PromptGlow { get; init; }
    // Absent (older engine) ⇒ false (fail-quiet).
    [JsonPropertyName("refused")] public bool Refused { get; init; }
    // Canonical panel token; absent ⇒ legacy boolean fallback (never straight to hidden).
    [JsonPropertyName("state")] public string? State { get; init; }
}

internal sealed record StatsDto
{
    [JsonPropertyName("tts")] public TtsStatsDto? Tts { get; init; }
    [JsonPropertyName("stt")] public SttStatsDto? Stt { get; init; }
    [JsonPropertyName("lifetime")] public LifetimeDto? Lifetime { get; init; }
    [JsonPropertyName("diarization")] public DiarizationStatsDto? Diarization { get; init; }
}

internal sealed record TtsStatsDto
{
    [JsonPropertyName("rtf_avg")] public double RtfAvg { get; init; }
    [JsonPropertyName("rtf_min")] public double RtfMin { get; init; }
    [JsonPropertyName("rtf_max")] public double RtfMax { get; init; }
    [JsonPropertyName("first_avg_ms")] public double FirstAvgMs { get; init; }
    [JsonPropertyName("first_min_ms")] public double FirstMinMs { get; init; }
    [JsonPropertyName("first_max_ms")] public double FirstMaxMs { get; init; }
    [JsonPropertyName("utterances")] public long Utterances { get; init; }
    [JsonPropertyName("audio_secs")] public double AudioSecs { get; init; }
    [JsonPropertyName("failures")] public long Failures { get; init; }
}

internal sealed record SttStatsDto
{
    [JsonPropertyName("rtf_avg")] public double RtfAvg { get; init; }
    [JsonPropertyName("rtf_min")] public double RtfMin { get; init; }
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

internal sealed record DiarizationStatsDto
{
    [JsonPropertyName("enabled")] public bool Enabled { get; init; }
    [JsonPropertyName("present")] public bool Present { get; init; }
    [JsonPropertyName("runtime")] public string? Runtime { get; init; }
    [JsonPropertyName("speakers")] public string?[]? Speakers { get; init; }
    [JsonPropertyName("clustering_threshold")] public double ClusteringThreshold { get; init; }
    [JsonPropertyName("speaker_threshold")] public double SpeakerThreshold { get; init; }
}

internal sealed record CapsEventDto
{
    [JsonPropertyName("ts")] public long Ts { get; init; }
    [JsonPropertyName("kind")] public string? Kind { get; init; }
}
