using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices.WindowsRuntime;
using System.Text.Json;
using System.Text.Json.Serialization;
using Microsoft.UI;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Documents;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace DontSpeak;

/// <summary>
/// The Fluent main window — mirrors the macOS SwiftUI StatusView (the "DontSpeak"
/// row with lifetime usage + version, the TTS/STT engine rows, and the
/// Caps-Lock row) and ToolsView (the MCP catalog). It polls the engine's
/// model-status over the C ABI and shows it; runtime control is via DontSpeak,
/// exactly as on macOS (the one in-window action is kicking off a model download). The
/// engine + tray are owned by <see cref="App"/>; closing this window hides it to the tray.
/// </summary>
public sealed partial class MainWindow : Window
{
    // Green/Red are the standard status colors (as on macOS: Color.green / .red); the
    // warming/downloading "Orange" dot is the shared brand WARNING tint (Brand.warning).
    private static readonly SolidColorBrush Green = new(Color.FromArgb(255, 46, 160, 67));
    private static readonly SolidColorBrush Orange = new(Brand.Warning);
    private static readonly SolidColorBrush Red = new(Color.FromArgb(255, 232, 70, 70));
    private static readonly SolidColorBrush Gray = new(Color.FromArgb(120, 150, 150, 155));
    // Windows 11's modern monospaced UI font (shipped since Win11; falls back to Consolas) —
    // for tool/param identifiers, the native analogue of macOS's SF Mono usage in ToolsView.
    private static readonly FontFamily Mono = new("Cascadia Mono, Consolas");

    public MainWindow()
    {
        InitializeComponent();
        // Open at the COMPACT width (== the minimum the user can narrow it to — the snug, "right"
        // width); the height is provisional and gets snugged to the content below.
        AppWindow.Resize(new Windows.Graphics.SizeInt32(380, 620));
        // Brand window/taskbar icon (the .ico is bundled next to the exe).
        var icoPath = System.IO.Path.Combine(AppContext.BaseDirectory, "AppIcon.ico");
        if (System.IO.File.Exists(icoPath)) AppWindow.SetIcon(icoPath);
        // Only the CLOSE button — no minimize/maximize. The window is WIDTH-resizable (drag the
        // side borders); the HEIGHT is LOCKED to the Status content (CapHeightToStatusContent), so
        // it always wraps the cards and can't be cut short. IsMaximizable/IsMinimizable=false alone
        // only GREY the buttons; StripMinMaxButtons() below clears the WS_*BOX styles to remove them.
        if (AppWindow.Presenter is Microsoft.UI.Windowing.OverlappedPresenter pr)
        {
            pr.IsResizable = true;        // width resize via the border (height is locked to content)
            pr.IsMaximizable = false;
            pr.IsMinimizable = false;
            pr.PreferredMinimumWidth = 380;
            pr.PreferredMinimumHeight = 240;   // provisional; CapHeightToStatusContent locks it to the content
        }
        StripMinMaxButtons();
        HookTitleBarTheme();
        // Match the overlaid state stripe's height to the top tab row once it's laid out,
        // and keep it matched if the row ever re-measures.
        Nav.Loaded += (_, _) => SizeStateStripe();
        Nav.SizeChanged += (_, _) => SizeStateStripe();
        LoadTools();
        LoadLibraries();
        RefreshStatus();

        // No poll timer: the status window is driven by App's status PUSH thread (the
        // dedicated ModelStatusWait loop), which calls ApplyPushed() on every engine status
        // change — the same ~0-jitter push that drives the tray + dictation overlay. This
        // one-shot refreshes the instant the window becomes visible; pushes skip work while
        // it's hidden (ApplyPushed early-returns), so a hidden window costs nothing and never
        // strands on a stale frame when reopened.
        AppWindow.Changed += (s, e) =>
        {
            if (e.DidVisibilityChange && s.IsVisible)
            {
                RefreshStatus();
                // Snug the height to the content once the window is shown + laid out. Low priority
                // so it runs AFTER the arrange pass (ActualHeight valid) and the window has its real
                // client size — at that point the provisional 620 is pulled down to the content.
                DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, CapHeightToStatusContent);
            }
        };

        // Re-cap on every later content/width change: the panel's ARRANGED size changing (a section
        // expands/collapses, an empty-state message wraps, the user resizes the WIDTH so text
        // re-wraps) fires SizeChanged post-arrange, so ActualHeight is valid (a manual Measure would
        // corrupt the live layout — that blanks the window).
        if (StatusScroll?.Content is FrameworkElement statusPanel)
            statusPanel.SizeChanged += (_, _) => CapHeightToStatusContent();
    }

    // The default WinUI 3 title bar does not follow the system dark/light theme on its
    // own — in dark mode it stays light, with dark (unreadable) caption buttons. Color the
    // title bar + caption buttons to match the window content's ActualTheme.
    private void HookTitleBarTheme()
    {
        if (Content is not FrameworkElement root) return;
        // ActualTheme is unreliable until the element is loaded (it reads Light in the
        // ctor even under system dark mode), so apply on Loaded — and again whenever the
        // system theme flips at runtime.
        ApplyTitleBarTheme(root.ActualTheme);
        root.Loaded += (_, _) => ApplyTitleBarTheme(root.ActualTheme);
        root.ActualThemeChanged += (s, _) => ApplyTitleBarTheme(s.ActualTheme);
    }

    private void ApplyTitleBarTheme(ElementTheme theme)
    {
        if (!Microsoft.UI.Windowing.AppWindowTitleBar.IsCustomizationSupported()) return;
        var tb = AppWindow.TitleBar;
        bool dark = theme == ElementTheme.Dark;
        Color bg = dark ? Color.FromArgb(255, 32, 32, 32) : Color.FromArgb(255, 243, 243, 243);
        Color fg = dark ? Colors.White : Colors.Black;
        Color inactiveFg = dark ? Color.FromArgb(255, 150, 150, 150) : Color.FromArgb(255, 120, 120, 120);
        Color hover = dark ? Color.FromArgb(255, 55, 55, 55) : Color.FromArgb(255, 225, 225, 225);
        Color pressed = dark ? Color.FromArgb(255, 70, 70, 70) : Color.FromArgb(255, 210, 210, 210);
        tb.BackgroundColor = bg;
        tb.ForegroundColor = fg;
        tb.InactiveBackgroundColor = bg;
        tb.InactiveForegroundColor = inactiveFg;
        tb.ButtonBackgroundColor = bg;
        tb.ButtonForegroundColor = fg;
        tb.ButtonInactiveBackgroundColor = bg;
        tb.ButtonInactiveForegroundColor = inactiveFg;
        tb.ButtonHoverBackgroundColor = hover;
        tb.ButtonHoverForegroundColor = fg;
        tb.ButtonPressedBackgroundColor = pressed;
        tb.ButtonPressedForegroundColor = fg;
    }

    // Remove the minimize/maximize caption buttons (leaving only close) by clearing
    // WS_MINIMIZEBOX/WS_MAXIMIZEBOX and refreshing the frame. WS_THICKFRAME (the resize border)
    // is left intact, so the window stays WIDTH-resizable.
    private void StripMinMaxButtons()
    {
        var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(this);
        long style = GetWindowLongPtr(hwnd, GWL_STYLE).ToInt64();
        SetWindowLongPtr(hwnd, GWL_STYLE, (IntPtr)(style & ~(WS_MINIMIZEBOX | WS_MAXIMIZEBOX)));
        SetWindowPos(hwnd, IntPtr.Zero, 0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED);
    }

    private const int GWL_STYLE = -16;
    private const long WS_MINIMIZEBOX = 0x00020000, WS_MAXIMIZEBOX = 0x00010000;
    private const uint SWP_NOSIZE = 0x0001, SWP_NOMOVE = 0x0002, SWP_NOZORDER = 0x0004, SWP_FRAMECHANGED = 0x0020;
    [System.Runtime.InteropServices.DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")]
    private static extern IntPtr GetWindowLongPtr(IntPtr hWnd, int nIndex);
    [System.Runtime.InteropServices.DllImport("user32.dll", EntryPoint = "SetWindowLongPtrW")]
    private static extern IntPtr SetWindowLongPtr(IntPtr hWnd, int nIndex, IntPtr dwNewLong);
    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern bool SetWindowPos(IntPtr hWnd, IntPtr after, int x, int y, int cx, int cy, uint flags);

    /// <summary>Select the Status (false) or Tools (true) tab — called by the tray.</summary>
    public void SelectTab(bool tools)
    {
        int i = tools ? 1 : 0;
        if (Nav.MenuItems.Count > i) Nav.SelectedItem = Nav.MenuItems[i];
    }

    private void Nav_SelectionChanged(NavigationView sender, NavigationViewSelectionChangedEventArgs args)
    {
        var tag = (args.SelectedItem as NavigationViewItem)?.Tag as string;
        if (StatusScroll != null) StatusScroll.Visibility = tag == "status" ? Visibility.Visible : Visibility.Collapsed;
        if (ToolsScroll != null) ToolsScroll.Visibility = tag == "tools" ? Visibility.Visible : Visibility.Collapsed;
        if (CreditsScroll != null) CreditsScroll.Visibility = tag == "credits" ? Visibility.Visible : Visibility.Collapsed;
        if (LogTab != null) LogTab.Visibility = tag == "log" ? Visibility.Visible : Visibility.Collapsed;
        if (tag == "log") LoadLogs(); // reload each time the tab is shown (no poll timer)
    }

    // ── Logs tab: the COMBINED activity log with a top text-filter bar ───────────────────────
    private List<LogLine> _logLines = new();
    private List<string> _logSources = new();                          // distinct sources (for stable per-source color)
    private readonly Dictionary<string, SolidColorBrush> _sourceBrush = new();
    private readonly Dictionary<string, SolidColorBrush> _levelBrushCache = new();
    private string _logFilter = "";                                    // free-text filter (case-insensitive substring)
    // The per-source palette + ERROR/WARN colors come from the SHARED Rust source via Brand
    // (Brand.LogSourcePalette / Brand.LogLevelColor) — centralized beside the brand colors.

    /// <summary>Load the COMBINED activity log (unified + aux logs, via <c>ds_logs_json</c>)
    /// and render. Called on every Logs tab-select, so it's fresh without a poll timer. Lines stay
    /// color-coded (per source + ERROR/WARN) and are narrowed live by the top filter bar.</summary>
    private void LoadLogs()
    {
        if (LogText == null) return;
        _logLines = ParseLogs(Native.LogsJson(64 * 1024));
        // Distinct sources in first-appearance order → stable per-source colors.
        _logSources = new List<string>();
        foreach (var l in _logLines)
            if (l.Source.Length > 0 && !_logSources.Contains(l.Source)) _logSources.Add(l.Source);
        RenderLogLines();
    }

    private void LogFilter_TextChanged(object sender, TextChangedEventArgs e)
    {
        _logFilter = LogFilter.Text ?? "";
        RenderLogLines();
    }

    /// <summary>Render the lines matching the text filter into the RichTextBlock: a colored source
    /// tag, the level token when it's not the ordinary INFO, and the message (red/amber for
    /// ERROR/WARN). The filter is a case-insensitive substring over source/level/message.
    /// Auto-scrolls to the newest line.</summary>
    private void RenderLogLines()
    {
        LogText.Blocks.Clear();
        var q = _logFilter.Trim();
        var shown = _logLines.Where(l => q.Length == 0
            || l.Text.Contains(q, StringComparison.OrdinalIgnoreCase)
            || l.Source.Contains(q, StringComparison.OrdinalIgnoreCase)
            || l.Level.Contains(q, StringComparison.OrdinalIgnoreCase)).ToList();
        if (shown.Count == 0)
        {
            var empty = new Paragraph { Margin = new Thickness(0) };
            empty.Inlines.Add(new Run { Text = Loc.T(_logLines.Count == 0 ? "logs.empty" : "logs.no_match"), Foreground = Gray });
            LogText.Blocks.Add(empty);
            return;
        }
        foreach (var l in shown)
        {
            var para = new Paragraph { Margin = new Thickness(0) };
            para.Inlines.Add(new Run { Text = l.Source, Foreground = SourceBrush(l.Source), FontWeight = FontWeights.SemiBold });
            para.Inlines.Add(new Run { Text = "  " });
            var msgBrush = LevelBrush(l.Level);
            if (l.Level.Length > 0 && l.Level != "INFO")
                para.Inlines.Add(new Run { Text = l.Level + " ", Foreground = msgBrush ?? Gray });
            var msg = new Run { Text = l.Text };
            if (msgBrush != null) msg.Foreground = msgBrush; // ERROR/WARN tint the message; INFO stays default
            para.Inlines.Add(msg);
            LogText.Blocks.Add(para);
        }
        DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low,
            () => LogScroll?.ChangeView(null, LogScroll.ScrollableHeight, null, true));
    }

    // Level color from the shared source (Brand.LogLevelColor): ERROR/WARN → its color, INFO/
    // unknown → null (inherit the default text color). Cached as brushes.
    private SolidColorBrush? LevelBrush(string level)
    {
        if (level.Length == 0) return null;
        if (_levelBrushCache.TryGetValue(level, out var b)) return b;
        if (Brand.LogLevelColor(level) is not Color c) return null;
        var brush = new SolidColorBrush(c);
        _levelBrushCache[level] = brush;
        return brush;
    }

    // Stable per-source color from the SHARED palette (Brand.LogSourcePalette), keyed by the
    // source's first-appearance index — identical on every platform reading the same lines.
    private SolidColorBrush SourceBrush(string source)
    {
        if (_sourceBrush.TryGetValue(source, out var b)) return b;
        var palette = Brand.LogSourcePalette;
        var color = palette.Length == 0
            ? Gray.Color
            : palette[Math.Max(0, _logSources.IndexOf(source)) % palette.Length];
        var brush = new SolidColorBrush(color);
        _sourceBrush[source] = brush;
        return brush;
    }

    private List<LogLine> ParseLogs(string json)
    {
        if (string.IsNullOrWhiteSpace(json)) return new();
        try
        {
            var raw = JsonSerializer.Deserialize<List<LogLineRaw>>(json, ToolsJsonOptions);
            return raw?.Select(d => new LogLine(d.Source ?? "", d.Level ?? "", d.Text ?? "")).ToList() ?? new();
        }
        catch { return new(); }
    }

    private readonly record struct LogLine(string Source, string Level, string Text);

    private sealed record LogLineRaw(
        [property: JsonPropertyName("source")] string? Source,
        [property: JsonPropertyName("level")] string? Level,
        [property: JsonPropertyName("text")] string? Text);

    private bool _refreshing;

    /// <summary>Poll the engine status WITHOUT blocking the UI thread: the C-ABI
    /// model-status read (which also probes NVML) runs on the thread pool; the await
    /// resumes on the dispatcher to apply the UI updates.</summary>
    private async void RefreshStatus()
    {
        // Skip while hidden (the app sits in the tray most of the time): no point probing
        // the engine or rebuilding UI nobody can see — App's own tray poll keeps running,
        // and AppWindow.Changed (ctor) refreshes the instant the window is shown again, so
        // it never strands on an old frame.
        if (!AppWindow.IsVisible) return;
        if (_refreshing) return;          // never queue behind a slow read
        _refreshing = true;
        HealthSnapshot? snap = null;
        try
        {
            var probe = System.Threading.Tasks.Task.Run(HealthSnapshot.Probe);
            // Bound the in-process engine round-trip: a single status read that doesn't
            // return must NOT keep `_refreshing` latched — that would skip every later
            // tick and freeze the whole window (GPU row included), not just the stats.
            var done = await System.Threading.Tasks.Task.WhenAny(
                probe, System.Threading.Tasks.Task.Delay(2500));
            if (done == probe) snap = await probe;
        }
        catch { /* probe threw this cycle — retry on the next tick */ }
        finally { _refreshing = false; }

        // Timed out / failed: keep the last frame and try again next tick. Wrap the
        // render so one bad frame can never kill the poll loop.
        if (snap is null) return;
        try { ApplyStatus(snap); } catch { /* one bad frame must not kill the loop */ }
    }

    /// <summary>Apply a status snapshot PUSHED by App's WaitModelStatus thread — already on the
    /// UI thread (TryEnqueue) and already projected, so no probe here. Skipped while the window
    /// is hidden (the usual tray state); the AppWindow.Changed one-shot re-renders on show, so a
    /// hidden window costs nothing.</summary>
    internal void ApplyPushed(HealthSnapshot s)
    {
        if (!AppWindow.IsVisible) return;
        try { ApplyStatus(s); } catch { /* one bad frame must not kill the push */ }
    }

    private void ApplyStatus(HealthSnapshot s)
    {
        // ── 1. DontSpeak — dot + the expanded lifetime usage / version ──
        EngineDot.Fill = s.Activity.EngineRunning ? Green : Gray;
        TtsAllTime.Text = Native.DurationLive(s.Lifetime.TtsSecs);
        SttAllTime.Text = Native.DurationLive(s.Lifetime.SttSecs);
        var v = Native.Version();
        VersionText.Text = v.Length > 0 ? v : Loc.T("common.dash");

        // ── 2. Engine rows — TTS / STT name the CONCRETE engine for the active token
        // (mirrors the macOS ttsEngineRow/sttEngineRow). ──
        if (s.EngineSelection.TtsEngine == "off")
        { TtsDetail.Text = ""; ApplyOff(TtsDot); }   // no label — the gray dot says "off"
        else if (s.EngineSelection.TtsEngine == "system")
        { TtsDetail.Text = Loc.T("status.engine.system"); ApplyEngine(s.EngineDots.TtsSystem, TtsDot); }
        else
        { TtsDetail.Text = Loc.T("status.engine.kokoro"); ApplyEngine(s.EngineDots.Kokoro, TtsDot); }

        switch (s.EngineSelection.SttEngine)
        {
            case "off":
                SttDetail.Text = "";   // no label — the gray dot says "off"
                ApplyOff(SttDot); break;
            case "claude_code":
                SttDetail.Text = Loc.T("status.engine.claude_code");
                ApplyEngine(s.EngineDots.ClaudeCode, SttDot); break;
            case "system":
                SttDetail.Text = Loc.T("status.engine.system");
                ApplyEngine(s.EngineDots.System, SttDot); break;
            default:
                SttDetail.Text = Loc.T("status.engine.parakeet");
                ApplyEngine(s.EngineDots.Parakeet, SttDot); break;
        }

        // TTS expansion. A not-ready engine (downloading/starting/failed) shows its state word
        // as the note — the same slot the stats/empty-states use (replaces the dot tooltip).
        // System `say` synthesizes in the OS (a pointer to the OS voices); else lead with the
        // active runtime (ORT CPU/CUDA/Core ML · ANE) then no-data / the live Kokoro stats.
        bool ttsSystem = s.EngineSelection.TtsEngine == "system";
        TtsRuntimeRow.Visibility = (!ttsSystem && s.EngineSelection.TtsProvider.Length > 0) ? Visibility.Visible : Visibility.Collapsed;
        if (!ttsSystem) TtsRuntimeText.Text = Native.RuntimeLabel(s.EngineSelection.TtsProvider);
        TtsSystemSettingsRow.Visibility = Visibility.Collapsed;   // default; only the System branch shows it
        var ttsInfo = s.ActiveTts;
        if (IsTrouble(ttsInfo.State))
            ShowMsg(TtsStatsMsg, TtsStatsGrid, ttsInfo.Word);
        else if (ttsSystem)
            ShowSystemVoiceLink();   // OS does the synth — no local stats; offer "Manage voices" instead
        else if (s.Tts.Utterances == 0)
            ShowMsg(TtsStatsMsg, TtsStatsGrid, Loc.T("status.no_data"));
        else
        {
            ShowGrid(TtsStatsMsg, TtsStatsGrid);
            TtsSpeed.Text = Native.StatsRange(s.Tts.RtfMin, s.Tts.RtfAvg, s.Tts.RtfMax, 2, "status.stats.unit.times");
            TtsFirst.Text = Native.StatsRange(s.Tts.FirstMinMs / 1000, s.Tts.FirstAvgMs / 1000, s.Tts.FirstMaxMs / 1000, 1, "status.stats.unit.seconds");
            TtsSpoken.Text = Native.StatsCount((ulong)s.Tts.Utterances, s.Tts.AudioSecs);
            TtsFailuresRow.Visibility = s.Tts.Failures > 0 ? Visibility.Visible : Visibility.Collapsed;
            if (s.Tts.Failures > 0) TtsFailures.Text = s.Tts.Failures.ToString();
        }

        // STT expansion. A not-ready engine shows its state word as the note (same slot as the
        // stats). The runtime line shows for built_in (Parakeet) only; Claude Code does no local
        // transcription, so when ready it names the key it delegates through instead of stats.
        bool sttBuiltIn = s.EngineSelection.SttEngine == "built_in";
        SttRuntimeRow.Visibility = (sttBuiltIn && s.EngineSelection.SttProvider.Length > 0) ? Visibility.Visible : Visibility.Collapsed;
        if (sttBuiltIn) SttRuntimeText.Text = Native.RuntimeLabel(s.EngineSelection.SttProvider);
        var sttInfo = s.ActiveStt;
        if (IsTrouble(sttInfo.State))
            ShowMsg(SttStatsMsg, SttStatsGrid, sttInfo.Word);
        else if (s.EngineSelection.SttEngine == "claude_code")
            ShowMsg(SttStatsMsg, SttStatsGrid, ClaudeDelegationHint(s));
        else if (s.Stt.Transcriptions == 0)
            ShowMsg(SttStatsMsg, SttStatsGrid, Loc.T("status.no_data"));
        else
        {
            ShowGrid(SttStatsMsg, SttStatsGrid);
            SttSpeed.Text = Native.StatsRange(s.Stt.RtfMin, s.Stt.RtfAvg, s.Stt.RtfMax, 2, "status.stats.unit.times");
            SttTranscribed.Text = Native.StatsCount((ulong)s.Stt.Transcriptions, s.Stt.AudioSecs);
        }

        // The top tab row reflects the live engine state — idle / recording (STT) /
        // speaking (TTS) — washing the row in the same brand tints as the tray icon.
        ApplyStateAccent(s.IndicatorState());

        // ── 3. Caps-Lock dictation ──
        CapsDot.Fill = s.Activity.Caps ? Green : Gray;
    }

    private TrayIcon.IconState _accentState = (TrayIcon.IconState)(-1);

    /// <summary>Size the overlaid <c>StateStripe</c> to the top tab row's height (it's a
    /// VerticalAlignment=Top overlay, so it needs an explicit height to fill the bar). Reads
    /// the live height of the NavigationView's top-nav grid; falls back to the WinUI default
    /// top-pane height if the template part can't be found.</summary>
    private void SizeStateStripe()
    {
        double h = 48; // WinUI top NavigationView pane height (fallback)
        if (FindDescendant(Nav, "TopNavGrid") is FrameworkElement bar && bar.ActualHeight > 0)
            h = bar.ActualHeight;
        StateStripe.Height = h;
    }

    /// <summary>Depth-first search of the visual tree for a descendant with the given name.</summary>
    private static FrameworkElement? FindDescendant(DependencyObject root, string name)
    {
        int n = Microsoft.UI.Xaml.Media.VisualTreeHelper.GetChildrenCount(root);
        for (int i = 0; i < n; i++)
        {
            var child = Microsoft.UI.Xaml.Media.VisualTreeHelper.GetChild(root, i);
            if (child is FrameworkElement fe && fe.Name == name) return fe;
            if (FindDescendant(child, name) is FrameworkElement hit) return hit;
        }
        return null;
    }

    /// <summary>Highlight the WHOLE top bar with the brand tint for the current engine state —
    /// idle = no fill, recording = orange STT, speaking = purple TTS. A uniform translucent
    /// wash edge-to-edge (kept low so the Status / Tools tabs stay readable through it), so the
    /// state reads as a consistent bar highlight. Reuses the SAME <see cref="Brand"/> tints as
    /// the tray icon.</summary>
    private void ApplyStateAccent(TrayIcon.IconState state)
    {
        if (state == _accentState) return;
        _accentState = state;

        var tint = state switch
        {
            TrayIcon.IconState.Recording => Brand.MicOrange,
            TrayIcon.IconState.Speaking => Brand.SeedPurple,
            _ => (Windows.UI.Color?)null,
        };
        // Idle: no stripe at all (clear the fill, leaving the bare tab row).
        if (tint is not Windows.UI.Color basis)
        {
            StateStripe.Background = null;
            return;
        }
        // A uniform translucent tint across the whole top bar — low enough (~30%) that the
        // Status / Tools tabs stay readable through it and it blends with the Mica, rather than
        // a solid slab. Idle clears it (handled above).
        const double Tint = 0.30;
        StateStripe.Background = new Microsoft.UI.Xaml.Media.SolidColorBrush(
            Windows.UI.Color.FromArgb((byte)(255 * Tint), basis.R, basis.G, basis.B));
    }

    /// <summary>Apply an engine's lifecycle to its dot (color only — no tooltip). A not-ready
    /// engine surfaces its state word as a note in the expanded section instead. Models fetch
    /// automatically on first activation, so there's no Download/Retry button — the dot conveys
    /// missing → downloading → running by color, matching macOS.</summary>
    private static void ApplyEngine(EngineInfo e, Microsoft.UI.Xaml.Shapes.Ellipse dot)
    {
        dot.Fill = e.State switch
        {
            EngineState.Running => Green,
            EngineState.Warming or EngineState.Downloading => Orange,
            EngineState.Failed => Red,
            _ => Gray,
        };
    }

    /// <summary>An engine switched off (tts_engine/stt_engine = off): just a gray idle dot, no
    /// label (mirrors the macOS offEngineRow). No tooltip — like every dot now.</summary>
    private static void ApplyOff(Microsoft.UI.Xaml.Shapes.Ellipse dot)
    {
        dot.Fill = Gray;
    }

    /// <summary>Claude Code does no local transcription (it delegates), so its ready STT row
    /// names the synthesized key it sends through instead of stats.</summary>
    private static string ClaudeDelegationHint(HealthSnapshot s) =>
        s.EngineSelection.ClaudeCodeKey.Length > 0
            ? Loc.T("status.stt_claude_code", new Dictionary<string, string> { ["key"] = s.EngineSelection.ClaudeCodeKey })
            : Loc.T("status.stt_claude_code_off");

    /// <summary>A not-ready engine — fetching its model, starting up, or failed — whose state
    /// word is shown as a note in the expanded section (where the stats go), replacing the old
    /// dot tooltip. Idle/Running are the ready states (stats shown).</summary>
    private static bool IsTrouble(EngineState st) =>
        st is EngineState.Missing or EngineState.Downloading or EngineState.Warming or EngineState.Failed;

    private static void ShowMsg(TextBlock msg, FrameworkElement grid, string text)
    {
        msg.Text = text; msg.Visibility = Visibility.Visible; grid.Visibility = Visibility.Collapsed;
    }
    private static void ShowGrid(TextBlock msg, FrameworkElement grid)
    {
        msg.Visibility = Visibility.Collapsed; grid.Visibility = Visibility.Visible;
    }
    // System TTS expansion: the OS synthesizes (no local stats), so show the clickable
    // "Manage voices" link to the OS voice-settings page instead of the message/stats slots.
    private void ShowSystemVoiceLink()
    {
        TtsSystemSettingsText.Text = Loc.T("status.tts_system_settings");
        TtsStatsMsg.Visibility = Visibility.Collapsed;
        TtsStatsGrid.Visibility = Visibility.Collapsed;
        TtsSystemSettingsRow.Visibility = Visibility.Visible;
    }
    // RTF/first-audio ranges and the count+seconds string are now the SHARED Rust formatters
    // (Native.StatsRange / Native.StatsCount), so the assembly + catalog keys live in ONE place
    // for all three hosts — see ds-core status_fmt. The old per-platform Range()/CountText()
    // helpers were removed.

    // The version label is a link to the product homepage (dontspeak.org) — the shared URL
    // from the Rust core, same as the macOS app. The HyperlinkButton consumes its own click,
    // so the surrounding row's tap-to-expand is unaffected.
    private async void VersionLink_Click(object sender, RoutedEventArgs e)
    {
        var url = Native.HomepageUrl();
        if (url.Length > 0 && Uri.TryCreate(url, UriKind.Absolute, out var uri))
            await Windows.System.Launcher.LaunchUriAsync(uri);
    }

    // Open the OS voice-settings page (Windows: Time & language ▸ Speech) through the SHARED
    // Rust seam (ds_open_voice_settings) every platform UI calls — the macOS app routes
    // its Spoken-Content row to the same function.
    private void TtsSystemSettings_Click(object sender, RoutedEventArgs e) => Native.OpenVoiceSettings();

    // Tap a header row to expand/collapse its details (no chevron — the whole row toggles).
    private void DontSpeakHeader_Tapped(object sender, Microsoft.UI.Xaml.Input.TappedRoutedEventArgs e) => ToggleStats(DontSpeakStats);
    private void TtsHeader_Tapped(object sender, Microsoft.UI.Xaml.Input.TappedRoutedEventArgs e) => ToggleStats(TtsStats);
    private void SttHeader_Tapped(object sender, Microsoft.UI.Xaml.Input.TappedRoutedEventArgs e) => ToggleStats(SttStats);
    private void CapsHeader_Tapped(object sender, Microsoft.UI.Xaml.Input.TappedRoutedEventArgs e) => ToggleStats(CapsStats);
    private void ToggleStats(FrameworkElement panel)
    {
        panel.Visibility = panel.Visibility == Visibility.Visible ? Visibility.Collapsed : Visibility.Visible;
        // (The Status content panel's SizeChanged re-caps the window height after this re-layouts.)
    }

    // Top tab strip + the Grid's top (12) and bottom (20 == the side padding) margins around the
    // Status panel — the vertical chrome added to the measured content height to get the client
    // height. (Bottom == sides, so the window's bottom gap matches its left/right gaps.)
    private const double StatusChromeDip = 84;

    // The client height we last auto-fit the window to (-1 = never). If the window still matches it,
    // the user hasn't manually resized, so we keep wrapping the content; otherwise we respect their
    // (taller) size. See CapHeightToStatusContent.
    private int _lastFitClientPx = -1;

    /// <summary>Cap the window HEIGHT to the Status tab's content — all sections + a bottom padding
    /// equal to the 20px sides. The window can shrink (the content then scrolls) but can't be
    /// dragged TALLER than the content, so there's never empty space below the cards. WIDTH stays
    /// freely resizable. Snugs the current height down when it exceeds the content (after a
    /// collapse, or after widening the window so text re-wraps shorter). Measures at the current
    /// width so wrapping matches what's shown.</summary>
    private void CapHeightToStatusContent()
    {
        if (AppWindow.Presenter is not Microsoft.UI.Windowing.OverlappedPresenter pr) return;
        if (StatusScroll?.Content is not FrameworkElement panel || Content?.XamlRoot is null) return;
        double scale = Content.XamlRoot.RasterizationScale;
        // Use the panel's ALREADY-ARRANGED height (it's top-aligned, so it sizes to its content even
        // when the ScrollViewer is scrolling it) — measuring it by hand corrupts the live layout.
        // 0 while the Status tab is hidden (collapsed) or before first layout: nothing to cap yet.
        if (scale <= 0 || panel.ActualHeight <= 0) return;
        int clientPx = (int)Math.Ceiling((panel.ActualHeight + StatusChromeDip) * scale);
        int nonClientPx = Math.Max(0, AppWindow.Size.Height - AppWindow.ClientSize.Height);   // title bar
        // FLOOR the height at the content (min): the window can't be dragged SHORTER than the
        // content, so sections never get cut off. NO ceiling (max = null): it's freely resizable
        // TALLER. Width stays freely resizable too.
        pr.PreferredMinimumHeight = clientPx + nonClientPx;
        pr.PreferredMaximumHeight = null;
        // Auto-fit the height to the content (grow on expand, shrink on collapse, re-wrap on a width
        // change) UNLESS the user has dragged it taller — then keep their size, only growing if an
        // expanded section would otherwise be cut. `_lastFitClientPx` is what we last auto-fit to; if
        // the window still matches it, the user hasn't manually resized, so we keep tracking content.
        // (Resizing only the height fires no SizeChanged — the panel width is unchanged — so no loop.)
        int cur = AppWindow.ClientSize.Height;
        bool atAutoFit = _lastFitClientPx < 0 || Math.Abs(cur - _lastFitClientPx) <= 2;
        if (atAutoFit || cur < clientPx)
        {
            if (Math.Abs(cur - clientPx) > 2)
                AppWindow.ResizeClient(new Windows.Graphics.SizeInt32(AppWindow.ClientSize.Width, clientPx));
            _lastFitClientPx = clientPx;
        }
    }

    /// <summary>Build the Tools list from the shared MCP catalog (mirrors ToolsView). The
    /// catalog (`ds_tools_json` → the ds-tools crate's `catalog_ui`) is decoded
    /// type-safely into <see cref="ToolDto"/> records, then rendered.</summary>
    private void LoadTools()
    {
        string json = Native.ToolsJson();
        if (string.IsNullOrWhiteSpace(json)) return;
        List<ToolDto>? tools;
        try { tools = JsonSerializer.Deserialize<List<ToolDto>>(json, ToolsJsonOptions); }
        catch { return; }
        if (tools is null) return;

        foreach (var tool in tools)
        {
            var name = tool.Name ?? "";
            if (name.Length == 0) continue;

            // One native Fluent Expander per tool — collapsed by default, with the system
            // chevron + expand/collapse animation + card chrome. The Windows 11 analogue of
            // the macOS ToolsView disclosure rows (header = the tool name; expanding reveals
            // the summary + arguments). The catalog order is the authored display order (same
            // source the macOS ToolsView reads), so render as-is.
            var body = new StackPanel { Spacing = 10 };
            var desc = tool.Description ?? "";
            if (desc.Length > 0)
                body.Children.Add(new TextBlock { Text = desc, TextWrapping = TextWrapping.Wrap, Opacity = 0.75 });

            var ps = tool.Params ?? new List<ToolParamDto>();
            if (ps.Count == 0)
            {
                body.Children.Add(new TextBlock { Text = Loc.T("tools.no_arguments"), FontSize = 12, Opacity = 0.5 });
            }
            else
            {
                body.Children.Add(new TextBlock
                {
                    Text = Loc.T("tools.arguments").ToUpperInvariant(),
                    FontSize = 11,
                    FontWeight = FontWeights.SemiBold,
                    Opacity = 0.5,
                    CharacterSpacing = 60,   // a touch of tracking — the Fluent caption/overline look
                });
                foreach (var p in ps)
                {
                    var pname = p.Name ?? "";
                    if (pname.Length == 0) continue;

                    var head = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6 };
                    head.Children.Add(new TextBlock { Text = pname, FontFamily = Mono, FontSize = 13, FontWeight = FontWeights.Medium, VerticalAlignment = VerticalAlignment.Center });
                    head.Children.Add(new TextBlock { Text = string.IsNullOrEmpty(p.Type) ? "any" : p.Type, FontSize = 12, Opacity = 0.6, VerticalAlignment = VerticalAlignment.Center });
                    var req = new TextBlock { Text = p.Required ? Loc.T("tools.param.required") : Loc.T("tools.param.optional"), FontSize = 12, VerticalAlignment = VerticalAlignment.Center };
                    if (p.Required) req.Foreground = Orange; else req.Opacity = 0.6;   // required pops in the brand caution tint
                    head.Children.Add(req);
                    var detail = ParamDetail(p);
                    if (detail.Length > 0)
                        head.Children.Add(new TextBlock { Text = detail, FontSize = 12, Opacity = 0.6, VerticalAlignment = VerticalAlignment.Center });

                    var prow = new StackPanel { Spacing = 1 };
                    prow.Children.Add(head);
                    var pdesc = p.Description ?? "";
                    if (pdesc.Length > 0)
                        prow.Children.Add(new TextBlock { Text = pdesc, FontSize = 12, Opacity = 0.55, TextWrapping = TextWrapping.Wrap });
                    body.Children.Add(prow);
                }
            }

            ToolsList.Children.Add(new Expander
            {
                HorizontalAlignment = HorizontalAlignment.Stretch,
                HorizontalContentAlignment = HorizontalAlignment.Stretch,
                Header = new TextBlock { Text = name, FontFamily = Mono, FontWeight = FontWeights.SemiBold },
                Content = body,
            });
        }
    }

    private static readonly JsonSerializerOptions ToolsJsonOptions = new() { PropertyNameCaseInsensitive = true };

    /// <summary>Build the Libraries list from the SHARED Rust libraries catalog
    /// (<c>ds_libraries_json</c> → ds-model's <c>libraries::catalog</c>) — the
    /// downloaded models + runtimes, each with its license, collected from the same registry
    /// every platform fetches from, so the credits can't drift from what ships. One native
    /// Fluent Expander per project (header = name; expanding reveals what it's for, links to the
    /// project + the license — that link is labeled with the license name — and the files it
    /// fetches) — the same
    /// "expander list" look as the Tools tab.</summary>
    private void LoadLibraries()
    {
        string json = Native.LibrariesJson();
        if (string.IsNullOrWhiteSpace(json)) return;
        List<LibraryDto>? projects;
        try { projects = JsonSerializer.Deserialize<List<LibraryDto>>(json, ToolsJsonOptions); }
        catch { return; }
        if (projects is null) return;

        foreach (var p in projects)
        {
            var name = p.Name ?? "";
            if (name.Length == 0) continue;

            var body = new StackPanel { Spacing = 10 };

            var usage = p.Usage ?? "";
            if (usage.Length > 0)
                body.Children.Add(new TextBlock { Text = usage, TextWrapping = TextWrapping.Wrap, Opacity = 0.75 });

            // Project + license links — standard HyperlinkButtons (NavigateUri opens the
            // default browser on click; no code-behind needed), inline/zero-padding so they
            // read as links, not buttons.
            var links = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 16 };
            if (!string.IsNullOrEmpty(p.Homepage) && Uri.TryCreate(p.Homepage, UriKind.Absolute, out var hp))
                links.Children.Add(new HyperlinkButton { Content = Loc.T("libraries.homepage"), NavigateUri = hp, Padding = new Thickness(0), MinWidth = 0, MinHeight = 0 });
            // The license link is LABELED with the actual license (e.g. "MIT", "Apache-2.0"),
            // which used to sit as a chip on the collapsed header; falls back to the generic
            // "View License" only when the catalog has no license name.
            var lic = p.License ?? "";
            if (!string.IsNullOrEmpty(p.LicenseUrl) && Uri.TryCreate(p.LicenseUrl, UriKind.Absolute, out var lu))
                links.Children.Add(new HyperlinkButton { Content = lic.Length > 0 ? lic : Loc.T("libraries.view_license"), NavigateUri = lu, Padding = new Thickness(0), MinWidth = 0, MinHeight = 0 });
            if (links.Children.Count > 0) body.Children.Add(links);

            // The files this project downloads (name + size when known).
            var files = p.Files ?? new List<LicenseFileDto>();
            if (files.Count > 0)
            {
                body.Children.Add(new TextBlock
                {
                    Text = Loc.T("libraries.files").ToUpperInvariant(),
                    FontSize = 11,
                    FontWeight = FontWeights.SemiBold,
                    Opacity = 0.5,
                    CharacterSpacing = 60,
                });
                foreach (var f in files)
                {
                    var fname = f.Name ?? "";
                    if (fname.Length == 0) continue;
                    var row = new Grid
                    {
                        ColumnDefinitions =
                        {
                            new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) },
                            new ColumnDefinition { Width = GridLength.Auto },
                        },
                    };
                    row.Children.Add(new TextBlock { Text = fname, FontFamily = Mono, FontSize = 12, TextWrapping = TextWrapping.Wrap, Opacity = 0.8 });
                    if (f.SizeBytes is long sz && sz > 0)
                    {
                        var sizeTb = new TextBlock { Text = HumanSize(sz), FontSize = 12, Opacity = 0.5, HorizontalAlignment = HorizontalAlignment.Right, VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(8, 0, 0, 0) };
                        Grid.SetColumn(sizeTb, 1);
                        row.Children.Add(sizeTb);
                    }
                    body.Children.Add(row);
                }
            }

            // Header: just the project name — the license now rides the "view license" link in
            // the expanded body, so the collapsed header stays clean.
            var header = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, VerticalAlignment = VerticalAlignment.Center };
            header.Children.Add(new TextBlock { Text = name, FontWeight = FontWeights.SemiBold, VerticalAlignment = VerticalAlignment.Center });

            CreditsList.Children.Add(new Expander
            {
                HorizontalAlignment = HorizontalAlignment.Stretch,
                HorizontalContentAlignment = HorizontalAlignment.Stretch,
                Header = header,
                Content = body,
            });
        }
    }

    /// <summary>Bytes → a compact human size (GB/MB/KB) for the per-file size labels.</summary>
    private static string HumanSize(long bytes)
    {
        double b = bytes;
        if (b >= 1024d * 1024 * 1024) return $"{b / (1024d * 1024 * 1024):0.0} GB";
        if (b >= 1024d * 1024) return $"{b / (1024d * 1024):0.0} MB";
        if (b >= 1024d) return $"{b / 1024d:0} KB";
        return $"{bytes} B";
    }

    // Typed wire shape of the license catalog — mirrors the Rust ds-model
    // `libraries::catalog` source of truth (an ordered array of projects, each with an
    // ordered `files` array). Decoded case-insensitively; missing keys default.
    private sealed record LibraryDto(
        [property: JsonPropertyName("name")] string? Name,
        [property: JsonPropertyName("usage")] string? Usage,
        [property: JsonPropertyName("homepage")] string? Homepage,
        [property: JsonPropertyName("license")] string? License,
        [property: JsonPropertyName("license_url")] string? LicenseUrl,
        [property: JsonPropertyName("files")] List<LicenseFileDto>? Files);

    private sealed record LicenseFileDto(
        [property: JsonPropertyName("name")] string? Name,
        [property: JsonPropertyName("url")] string? Url,
        [property: JsonPropertyName("size_bytes")] long? SizeBytes);

    /// <summary>A constraint qualifier for a tool param, mirroring the macOS toToolParam:
    /// an enum becomes "one of: a, b, c"; a numeric min/max becomes "lo–hi". Empty when the
    /// param carries no constraint. Rendered as its own muted run in the param's header line.</summary>
    private static string ParamDetail(ToolParamDto p)
    {
        if (p.EnumValues is { Count: > 0 } vals)
            return Loc.T("tools.param.one_of",
                new Dictionary<string, string> { ["values"] = string.Join(", ", vals) });
        if (p.Minimum is double lo && p.Maximum is double hi)
            return $"{lo:0.##}–{hi:0.##}";
        return "";
    }

    // Typed wire shape of the tools catalog — mirrors the macOS ToolDTO/ParamDTO and the
    // Rust ds-tools `catalog_ui` source of truth (an ordered array of tools, each
    // with an ordered `params` array). Decoded case-insensitively; missing keys default.
    private sealed record ToolDto(
        [property: JsonPropertyName("name")] string? Name,
        [property: JsonPropertyName("description")] string? Description,
        [property: JsonPropertyName("params")] List<ToolParamDto>? Params);

    private sealed record ToolParamDto(
        [property: JsonPropertyName("name")] string? Name,
        [property: JsonPropertyName("type")] string? Type,
        [property: JsonPropertyName("required")] bool Required,
        [property: JsonPropertyName("description")] string? Description,
        [property: JsonPropertyName("enum")] List<string>? EnumValues,
        [property: JsonPropertyName("minimum")] double? Minimum,
        [property: JsonPropertyName("maximum")] double? Maximum);
}
