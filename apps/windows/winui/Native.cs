using System;
using System.Collections.Generic;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace DontSpeak;

/// <summary>
/// P/Invoke bridge to <c>ds_core.dll</c> — the SAME stable C ABI the macOS
/// SwiftUI app links (see <c>macos/Sources/CDontSpeak/include/dontspeak.h</c>). The
/// app hosts the engine IN-PROCESS: <see cref="EngineStart"/> on launch spins up
/// the caps loop + RPC server + TTS queue on a Rust background thread inside this
/// process; <see cref="EngineStop"/> tears it down on quit.
/// </summary>
internal static class Native
{
    private const string Dll = "ds_core.dll";

    [DllImport(Dll)] private static extern byte ds_engine_start();
    [DllImport(Dll)] private static extern byte ds_engine_stop();
    [DllImport(Dll)] private static extern byte ds_engine_running_global();
    [DllImport(Dll)] private static extern IntPtr ds_model_status_json();
    [DllImport(Dll)] private static extern IntPtr ds_model_status_wait(ulong since, uint timeoutMs);
    [DllImport(Dll)] private static extern IntPtr ds_tools_json();
    [DllImport(Dll)] private static extern IntPtr ds_libraries_json();
    [DllImport(Dll)] private static extern IntPtr ds_logs_json(uint maxBytes);
    [DllImport(Dll)] private static extern IntPtr ds_version();
    [DllImport(Dll)] private static extern IntPtr ds_homepage_url();
    [DllImport(Dll)] private static extern IntPtr ds_brand_colors_json();
    [DllImport(Dll)] private static extern IntPtr ds_log_colors_json();
    // Shared status-panel formatters (one implementation, every platform UI).
    [DllImport(Dll)] private static extern IntPtr ds_engine_state_word([MarshalAs(UnmanagedType.LPUTF8Str)] string state, double progress, [MarshalAs(UnmanagedType.LPUTF8Str)] string why);
    [DllImport(Dll)] private static extern IntPtr ds_duration_live(double secs);
    [DllImport(Dll)] private static extern IntPtr ds_runtime_label([MarshalAs(UnmanagedType.LPUTF8Str)] string provider);
    [DllImport(Dll)] private static extern IntPtr ds_stats_range(double lo, double avg, double hi, uint precision, [MarshalAs(UnmanagedType.LPUTF8Str)] string unitKey);
    [DllImport(Dll)] private static extern IntPtr ds_stats_count(ulong count, double audioSecs);
    [DllImport(Dll)] private static extern void ds_string_free(IntPtr s);
    [DllImport(Dll)] private static extern byte ds_set_muted(byte on);
    [DllImport(Dll)] private static extern byte ds_open_voice_settings();

    public static bool EngineStart() => ds_engine_start() != 0;
    public static bool EngineStop() => ds_engine_stop() != 0;
    public static bool EngineRunning() => ds_engine_running_global() != 0;

    /// <summary>Mute/unmute the voice (silences playback without stopping it) — the same C ABI
    /// the macOS tray "Mute" toggle calls. Returns the resulting muted state.</summary>
    public static bool SetMuted(bool on) => ds_set_muted((byte)(on ? 1 : 0)) != 0;

    /// <summary>Open the OS system-voice settings page (Windows: Time &amp; language ▸ Speech)
    /// via the SHARED Rust seam every platform UI calls — the System-TTS "Manage voices"
    /// affordance. Returns true if a page was launched.</summary>
    public static bool OpenVoiceSettings() => ds_open_voice_settings() != 0;

    /// <summary>Localized hover word for an engine lifecycle state (shared with macOS).</summary>
    public static string EngineStateWord(string state, double progress, string why) => TakeString(ds_engine_state_word(state, progress, why));

    /// <summary>Localized lifetime duration down to seconds (shared with macOS).</summary>
    public static string DurationLive(double secs) => TakeString(ds_duration_live(secs));

    /// <summary>Localized RUNTIME label for a resolved provider token (shared with macOS/Linux).</summary>
    public static string RuntimeLabel(string provider) => TakeString(ds_runtime_label(provider));

    /// <summary>A stat RANGE string "avg&lt;unit&gt;  ·  lo–hi" (shared formatter).</summary>
    public static string StatsRange(double lo, double avg, double hi, uint precision, string unitKey) => TakeString(ds_stats_range(lo, avg, hi, precision, unitKey));

    /// <summary>A COUNT + audio-duration stat string "&lt;count&gt;  &lt;secs&gt; s" (shared formatter).</summary>
    public static string StatsCount(ulong count, double audioSecs) => TakeString(ds_stats_count(count, audioSecs));

    /// <summary>The product version (shared Rust workspace version), e.g. "0.2.0".</summary>
    public static string Version() => TakeString(ds_version());

    /// <summary>The product homepage URL (dontspeak.org) — the SAME shared source of truth
    /// the macOS app links to; the version label opens it in the default browser.</summary>
    public static string HomepageUrl() => TakeString(ds_homepage_url());

    /// <summary>The brand tints as JSON ({seed_purple, mic_orange, warning}) — the SAME
    /// cross-platform source the macOS app reads (Brand.swift), so every UI tints
    /// identically. "{}" on the engine side; callers fall back to the brand hexes.</summary>
    public static string BrandColorsJson() => TakeString(ds_brand_colors_json());

    /// <summary>The Logs-tab colors as JSON ({levels, source_palette}) — the SAME shared Rust
    /// source every platform's Logs tab tints from. "{}" on the engine side; <see cref="Brand"/>
    /// falls back to the built-in palette.</summary>
    public static string LogColorsJson() => TakeString(ds_log_colors_json());

    /// <summary>The engine's model-status JSON ("{}" when the engine is down).</summary>
    public static string ModelStatusJson() => TakeString(ds_model_status_json());

    /// <summary>BLOCKS until the engine's status sequence differs from <paramref name="since"/>
    /// or <paramref name="timeoutMs"/> elapses, then returns the current model-status JSON
    /// (whose "seq" is the next <paramref name="since"/>). The push transport for the dictation
    /// overlay — call on a DEDICATED background thread (it blocks), never the UI thread. Pass
    /// since=0 first. "{}" when the engine is down.</summary>
    public static string ModelStatusWait(ulong since, uint timeoutMs) => TakeString(ds_model_status_wait(since, timeoutMs));

    /// <summary>The MCP tool catalog JSON, UI shape: an array of {name, description,
    /// params:[{name, type, required, description, …}]} in authored display order (the
    /// shared ds-tools catalog, same as the macOS ToolsView reads).</summary>
    public static string ToolsJson() => TakeString(ds_tools_json());

    /// <summary>The third-party libraries catalog JSON (downloaded models + runtimes), UI shape:
    /// an array of {name, usage, homepage, license, license_url, files:[{name, url, size_bytes?}]}.
    /// The SAME shared Rust catalog (ds-model) every platform's Libraries tab renders, so it
    /// can't drift from what's actually fetched.</summary>
    public static string LibrariesJson() => TakeString(ds_libraries_json());

    /// <summary>The COMBINED activity-log tail (last <paramref name="maxBytes"/> per file) for the
    /// Logs tab — a JSON array of {source, level, text} merging the unified log (tagged per
    /// subsystem) with every sibling aux log (e.g. the out-of-process "helper" stderr), in rough
    /// chronological order. Reads the SAME files the engine writes (shared-read). "[]" if no log
    /// yet. The UI derives its source filter from the distinct source values.</summary>
    public static string LogsJson(uint maxBytes) => TakeString(ds_logs_json(maxBytes));

    /// <summary>Copy a Rust-owned UTF-8 char* into a managed string and free it.</summary>
    private static string TakeString(IntPtr ptr)
    {
        if (ptr == IntPtr.Zero) return "";
        try { return Marshal.PtrToStringUTF8(ptr) ?? ""; }
        finally { ds_string_free(ptr); }
    }
}

/// <summary>Lifecycle state of one engine/model (mirrors the SwiftUI EngineStatus).</summary>
public enum EngineState { Missing, Idle, Downloading, Warming, Running, Failed }

// Word is resolved ONCE at parse time via the shared Rust formatter
// (ds_engine_state_word), so the state→word mapping lives in one place for
// every platform — no per-UI switch to drift from the macOS wording.
public readonly record struct EngineInfo(EngineState State, double Progress, string Word);

// The flat HealthSnapshot fields, grouped into cohesive sub-structs that MIRROR the macOS
// HealthSnapshot for cross-platform parity. The per-engine stats/loaded/lifetime totals
// (further down on HealthSnapshot) keep their own grouping, exactly as before.

/// <summary>Live activity flags — the engine's `running` map plus the tray-tint setting.</summary>
public sealed record Activity
{
    public bool EngineRunning;
    public bool Caps, Recording, Speaking;
    // Global mute (the tray "Mute" toggle): the voice is silenced but playback isn't stopped.
    // Drives the tray icon's muted slash + the menu checkmark (mirrors macOS).
    public bool Muted;
    // Which live states tint the tray icon — a SET of tokens, one per state: stt/tts (static)
    // or stt_animated/tts_animated (the breathing form, a macOS effect). Default ["stt",
    // "tts_animated"]; [] = never tint. Engine default; this is a fallback only.
    public string[] TrayIndicator = { "stt", "tts_animated" };
}

/// <summary>Per-engine lifecycle dots — every engine object the status JSON carries:
/// Kokoro/Parakeet are the local models; ClaudeCode/System/TtsSystem are the delegate/OS
/// engines (no downloadable model).</summary>
public sealed record EngineDots
{
    public EngineInfo Kokoro, Parakeet, ClaudeCode, System, TtsSystem;
}

/// <summary>The ACTIVE engine tokens + their runtime EPs — the TTS/STT rows adapt to these
/// (which concrete engine is named, which dot/stats render), exactly as the macOS StatusView.</summary>
public sealed record EngineSelection
{
    //   stt_engine: claude_code (delegate) | built_in (Parakeet) | system (OS recognizer)
    //   tts_engine: built_in (Kokoro)      | system  (OS voice)
    public string SttEngine = "claude_code", TtsEngine = "built_in";
    // The active runtime/EP token for the built_in models (cpu | cuda | coreml
    // | ane), shown as the "Runtime" line inside the Kokoro/Parakeet stats. Empty = none.
    public string SttProvider = "", TtsProvider = "";
    // The human key label DontSpeak synthesizes for Claude Code dictation (e.g. "Space");
    // shown in the claude_code STT hint instead of meaningless local stats. Empty if N/A.
    public string ClaudeCodeKey = "";
}

/// <summary>Dictation confirm-panel state (the `dictation` object): the live/final transcript,
/// whether it's awaiting the confirm tap, and whether this is the local (Parakeet) path — so
/// the overlay can appear the moment recording starts (see model_status).</summary>
public sealed record Dictation
{
    public string DictText = "";
    public bool DictAwaitingConfirm, DictLocalStt;
    // LIVE: is an editable text field focused to receive the paste right now? The engine
    // samples this each tick while the panel is up; the overlay tints its glow when false
    // ("no input to submit into"). True by default (fail-open; mirrors macOS).
    public bool DictHasTarget = true;
    // The engine's "speak now" glow decision (recording, nothing transcribed yet, not
    // awaiting confirm) — computed once in the core so this overlay and the macOS one
    // pulse identically. The no-target warning glow stays driven by DictHasTarget.
    public bool DictPromptGlow;
}

/// <summary>TTS realtime / first-audio / throughput stats for the expandable Kokoro row
/// (mirrors the macOS EngineStats.tts group).</summary>
public sealed record TtsStats
{
    public double RtfAvg, RtfMin, RtfMax;
    public double FirstAvgMs, FirstMinMs, FirstMaxMs;
    public double AudioSecs;
    public int Utterances, Failures;
}

/// <summary>STT realtime / throughput stats for the expandable Parakeet row
/// (mirrors the macOS EngineStats.stt group).</summary>
public sealed record SttStats
{
    public double RtfAvg, RtfMin, RtfMax, AudioSecs;
    public int Transcriptions;
}

/// <summary>Persisted lifetime totals (seconds spoken / heard), summed across all sessions —
/// shown under the Status "DontSpeak" row's expansion (mirrors the macOS EngineStats.lifetime).</summary>
public sealed record LifetimeStats
{
    public double TtsSecs, SttSecs;
}

/// <summary>Whether each local model is currently resident in memory
/// (mirrors the macOS EngineStats.loaded group).</summary>
public sealed record LoadedStats
{
    public bool Tts, Stt;
}

/// <summary>A parsed snapshot of the engine's model-status JSON (mirrors HealthSnapshot).</summary>
internal sealed class HealthSnapshot
{
    public Activity Activity = new();
    public EngineDots EngineDots = new();
    public EngineSelection EngineSelection = new();
    public Dictation Dictation = new();

    /// <summary>The engine object ACTUALLY doing TTS for the active tts_engine (Kokoro,
    /// or the OS voice when "system") — so dots/tooltips name what's really speaking.</summary>
    public EngineInfo ActiveTts => EngineSelection.TtsEngine == "system" ? EngineDots.TtsSystem : EngineDots.Kokoro;
    /// <summary>The engine object ACTUALLY doing STT for the active stt_engine
    /// (Parakeet for "built_in", else the Claude Code delegate or OS recognizer).</summary>
    public EngineInfo ActiveStt => EngineSelection.SttEngine switch
    {
        "claude_code" => EngineDots.ClaudeCode,
        "system" => EngineDots.System,
        _ => EngineDots.Parakeet,
    };

    /// <summary>The status-indicator state for the live engine — the ONE mapping shared by the
    /// tray icon and the window's state stripe, gated by the `tray_indicator` setting. A state
    /// colors when its token is present in either form (`stt`/`stt_animated`, `tts`/`tts_animated`);
    /// the breathing the `_animated` form adds is a macOS effect, so Windows just colors.</summary>
    public TrayIcon.IconState IndicatorState()
    {
        bool Colors(string state) =>
            Array.IndexOf(Activity.TrayIndicator, state) >= 0 ||
            Array.IndexOf(Activity.TrayIndicator, state + "_animated") >= 0;
        if (Activity.Recording && Colors("stt")) return TrayIcon.IconState.Recording;
        if (Activity.Speaking && Colors("tts")) return TrayIcon.IconState.Speaking;
        return TrayIcon.IconState.Idle;
    }
    // Per-engine stats for the expandable Kokoro/Parakeet rows, grouped into cohesive
    // sub-records that mirror the macOS EngineStats split (tts / stt / lifetime / loaded).
    public TtsStats Tts = new();
    public SttStats Stt = new();
    public LifetimeStats Lifetime = new();
    public LoadedStats Loaded = new();

    // The engine's push sequence (status.rs StatusGate): the app echoes it back as
    // `since` to the next ModelStatusWait so the call blocks until the NEXT change.
    public ulong StatusSeq;

    public static HealthSnapshot Probe() => FromJson(Native.ModelStatusJson());

    // System.Text.Json options for the model_status decode: case-insensitive (belt-and-
    // braces over the explicit [JsonPropertyName]s) and tolerant of unknown/missing members
    // (default behaviour) so a schema that grows on the Rust side never throws here.
    private static readonly JsonSerializerOptions ModelStatusJsonOptions = new()
    {
        PropertyNameCaseInsensitive = true,
    };

    /// <summary>Parse a model-status JSON string into a snapshot. Shared by the polling
    /// <see cref="Probe"/> and the push loop (which already holds the JSON from a blocking
    /// ModelStatusWait, so it must not re-fetch).</summary>
    public static HealthSnapshot FromJson(string json)
    {
        var s = new HealthSnapshot();
        if (string.IsNullOrWhiteSpace(json) || json == "{}") return s;
        try
        {
            var dto = JsonSerializer.Deserialize<ModelStatusDto>(json, ModelStatusJsonOptions);
            if (dto is null) return s; // malformed/empty payload → default snapshot (Activity.EngineRunning stays false)
            // Non-empty, well-formed JSON ⇒ the engine is up (matches the old walk, which set
            // Activity.EngineRunning=true the moment JsonDocument.Parse succeeded). A malformed payload
            // throws above and falls into catch → default snapshot, exactly as before.
            s.Activity.EngineRunning = true;
            s.StatusSeq = dto.Seq;

            if (dto.Running is { } r)
            {
                // Field names per dontspeakd's model_status_json "running" map.
                s.Activity.Caps = r.Caps;
                s.Activity.Recording = r.SttActive;
                s.Activity.Speaking = r.TtsActive;
                s.Activity.Muted = r.Muted;
            }
            // Only override the default {"stt","tts"} when the key is actually present
            // (absent ⇒ DTO field is null ⇒ keep the default), mirroring the old guard.
            if (dto.TrayIndicator is { } ti)
                s.Activity.TrayIndicator = ti.Where(t => t is not null).Cast<string>().ToArray();
            if (dto.Dictation is { } d)
            {
                s.Dictation.DictText = d.Text ?? "";
                s.Dictation.DictAwaitingConfirm = d.AwaitingConfirm;
                s.Dictation.DictLocalStt = d.LocalStt;
                // Fail-open: true unless the engine explicitly says false (missing ⇒ true).
                s.Dictation.DictHasTarget = d.HasPasteTarget ?? true;
                s.Dictation.DictPromptGlow = d.PromptGlow;
            }
            // Active-engine tokens + their runtime EPs drive which TTS/STT row renders
            // (default to the engine's own defaults so a partial payload still picks a row).
            s.EngineSelection.SttEngine = NonEmptyOr(dto.SttEngine, "claude_code");
            s.EngineSelection.TtsEngine = NonEmptyOr(dto.TtsEngine, "built_in");
            s.EngineSelection.SttProvider = dto.SttProvider ?? "";
            s.EngineSelection.TtsProvider = dto.TtsProvider ?? "";
            s.EngineSelection.ClaudeCodeKey = dto.ClaudeCodeKey ?? "";
            s.EngineDots.Kokoro = ToEngine(dto.Kokoro);
            s.EngineDots.Parakeet = ToEngine(dto.Parakeet);
            s.EngineDots.ClaudeCode = ToEngine(dto.ClaudeCode);
            s.EngineDots.System = ToEngine(dto.System);
            s.EngineDots.TtsSystem = ToEngine(dto.TtsSystem);
            if (dto.Stats is { } stats)
            {
                if (stats.Loaded is { } loaded)
                {
                    s.Loaded.Tts = loaded.Tts;
                    s.Loaded.Stt = loaded.Stt;
                }
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
                }
                if (stats.Lifetime is { } lt)
                {
                    s.Lifetime.TtsSecs = lt.TtsSecs;
                    s.Lifetime.SttSecs = lt.SttSecs;
                }
            }
        }
        catch { /* engine mid-write / malformed → treat as the empty snapshot */ }
        return s;
    }

    private static string NonEmptyOr(string? v, string fallback) =>
        string.IsNullOrEmpty(v) ? fallback : v;

    /// <summary>Map a decoded engine object → <see cref="EngineInfo"/>: the `state` string
    /// drives the <see cref="EngineState"/> enum (1:1 with dontspeakd's engine_obj states) and
    /// the hover word comes from the shared Rust formatter (one mapping for every UI). A
    /// missing object reads as Missing, exactly as the old TryGetProperty walk did.</summary>
    private static EngineInfo ToEngine(EngineObjDto? o)
    {
        if (o is null)
            return new EngineInfo(EngineState.Missing, 0, Native.EngineStateWord("missing", 0, ""));
        var state = o.State ?? "";
        var pct = o.Progress;
        var why = o.Error ?? "";
        var es = state switch
        {
            "running" => EngineState.Running,
            "idle" => EngineState.Idle,
            "warming" => EngineState.Warming,
            "failed" => EngineState.Failed,
            "downloading" => EngineState.Downloading,
            _ => EngineState.Missing,
        };
        return new EngineInfo(es, pct, Native.EngineStateWord(state, pct, why));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Wire DTOs mirroring the engine's `model_status` JSON. The single source of truth for this
// schema is the Rust `ds-status` crate (rust/crates/ds-status/src/lib.rs); the
// engine builds it in dontspeakd/src/status.rs. These C# DTOs are a HAND mirror of that shape,
// kept honest by ds-status's round-trip contract test (a deliberately small,
// dependency-free boundary instead of codegen). Keep them in lockstep with the Rust schema.
// Every key is pinned with [JsonPropertyName] (no naming policy); unknown/missing members are
// tolerated, so a grown schema never throws. These types feed ONLY HealthSnapshot.FromJson —
// the public HealthSnapshot/EngineInfo shapes above are unchanged.

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
    // Nullable so "absent" (⇒ fail-open true) is distinguishable from an explicit false.
    [JsonPropertyName("has_paste_target")] public bool? HasPasteTarget { get; init; }
    [JsonPropertyName("prompt_glow")] public bool PromptGlow { get; init; }
}

internal sealed record StatsDto
{
    [JsonPropertyName("tts")] public TtsStatsDto? Tts { get; init; }
    [JsonPropertyName("stt")] public SttStatsDto? Stt { get; init; }
    [JsonPropertyName("lifetime")] public LifetimeDto? Lifetime { get; init; }
    [JsonPropertyName("loaded")] public LoadedDto? Loaded { get; init; }
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

internal sealed record LoadedDto
{
    [JsonPropertyName("tts")] public bool Tts { get; init; }
    [JsonPropertyName("stt")] public bool Stt { get; init; }
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
