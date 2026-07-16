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
/// Fluent Status/Tools/Logs/Credits window (macOS StatusView + ToolsView parity). Status is
/// push-driven by <see cref="App"/>; engine + tray live there. Close hides to tray.
/// </summary>
public sealed partial class MainWindow : Window
{
    // Green/red standard; Orange = Brand.Warning (warming/download), matching macOS.
    private static readonly SolidColorBrush Green = new(Color.FromArgb(255, 46, 160, 67));
    private static readonly SolidColorBrush Orange = new(Brand.Warning);
    private static readonly SolidColorBrush Red = new(Color.FromArgb(255, 232, 70, 70));
    private static readonly SolidColorBrush Gray = new(Color.FromArgb(120, 150, 150, 155));
    // Cascadia Mono (Win11) / Consolas — tools/params; macOS uses SF Mono.
    private static readonly FontFamily Mono = new("Cascadia Mono, Consolas");
    // Mirrors ds_tools::DIARIZATION_ENABLED — flip both when diarization ships.
    private const bool DiarizationUiEnabled = false;

    public MainWindow()
    {
        InitializeComponent();
        // Compact width (= min); height provisional until CapHeightToStatusContent.
        AppWindow.Resize(new Windows.Graphics.SizeInt32(380, 620));
        var icoPath = System.IO.Path.Combine(AppContext.BaseDirectory, "AppIcon.ico");
        if (System.IO.File.Exists(icoPath)) AppWindow.SetIcon(icoPath);
        // Close only; width-resizable; height locked to Status content.
        // IsMaximizable/IsMinimizable=false greys buttons — StripMinMaxButtons removes WS_*BOX.
        if (AppWindow.Presenter is Microsoft.UI.Windowing.OverlappedPresenter pr)
        {
            pr.IsResizable = true;
            pr.IsMaximizable = false;
            pr.IsMinimizable = false;
            pr.PreferredMinimumWidth = 380;
            pr.PreferredMinimumHeight = 240;
        }
        StripMinMaxButtons();
        HookTitleBarTheme();
        Nav.Loaded += (_, _) => SizeStateStripe();
        Nav.SizeChanged += (_, _) => SizeStateStripe();
        LoadTools();
        LoadLibraries();
        RefreshStatus();

        // No poll timer — App's push calls ApplyPushed. One-shot on show; pushes no-op while hidden.
        AppWindow.Changed += (s, e) =>
        {
            if (e.DidVisibilityChange && s.IsVisible)
            {
                RefreshStatus();
                // Low priority: after arrange so ActualHeight is valid.
                DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, CapHeightToStatusContent);
            }
        };

        // SizeChanged is post-arrange — don't Measure manually (corrupts layout → blank window).
        if (StatusScroll?.Content is FrameworkElement statusPanel)
            statusPanel.SizeChanged += (_, _) => CapHeightToStatusContent();
    }

    // WinUI 3 title bar ignores system theme by default (dark mode stays light/unreadable).
    private void HookTitleBarTheme()
    {
        if (Content is not FrameworkElement root) return;
        // ActualTheme wrong until Loaded (reads Light under system dark).
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

    // Drop WS_MINIMIZEBOX/MAXIMIZEBOX; keep WS_THICKFRAME for width resize.
    private void StripMinMaxButtons()
    {
        var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(this);
        long style = GetWindowLongPtr(hwnd, GWL_STYLE).ToInt64();
        // checked: CA2020 — makes IntPtr conversion intent explicit for the analyzer.
        SetWindowLongPtr(hwnd, GWL_STYLE, checked((IntPtr)(style & ~(WS_MINIMIZEBOX | WS_MAXIMIZEBOX))));
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

    public void SelectTab(bool tools)
    {
        int i = tools ? 1 : 0;
        if (Nav.MenuItems.Count > i) Nav.SelectedItem = Nav.MenuItems[i];
    }

    private async void Nav_SelectionChanged(NavigationView sender, NavigationViewSelectionChangedEventArgs args)
    {
        var tag = (args.SelectedItem as NavigationViewItem)?.Tag as string;
        int loadGeneration = ++_logLoadGeneration;
        ++_logRenderGeneration;
        if (StatusScroll != null) StatusScroll.Visibility = tag == "status" ? Visibility.Visible : Visibility.Collapsed;
        if (ToolsScroll != null) ToolsScroll.Visibility = tag == "tools" ? Visibility.Visible : Visibility.Collapsed;
        if (CreditsScroll != null) CreditsScroll.Visibility = tag == "credits" ? Visibility.Visible : Visibility.Collapsed;
        if (LogTab != null) LogTab.Visibility = tag == "log" ? Visibility.Visible : Visibility.Collapsed;
        if (tag == "log") await LoadLogsAsync(loadGeneration); // reload on each select (no poll)
    }

    private List<LogLine> _logLines = new();
    private List<string> _logSources = new();
    private readonly Dictionary<string, SolidColorBrush> _sourceBrush = new();
    private readonly Dictionary<string, SolidColorBrush> _levelBrushCache = new();
    private string _logFilter = "";
    private int _logLoadGeneration;
    private int _logRenderGeneration;

    /// <summary>Load combined log off UI thread; render in batches. Generation guard drops
    /// stale results if user left/re-entered Logs mid-flight.</summary>
    private async System.Threading.Tasks.Task LoadLogsAsync(int loadGeneration)
    {
        if (LogText == null) return;
        List<LogLine> lines;
        try
        {
            lines = await System.Threading.Tasks.Task.Run(
                () => LogParser.ParseLogs(Native.LogsJson(64 * 1024)));
        }
        catch { return; }
        if (loadGeneration != _logLoadGeneration || LogTab.Visibility != Visibility.Visible) return;
        _logLines = lines;
        _logSources = new List<string>();
        foreach (var l in _logLines)
            if (l.Source.Length > 0 && !_logSources.Contains(l.Source)) _logSources.Add(l.Source);
        await RenderLogLinesAsync(++_logRenderGeneration);
    }

    private async void LogFilter_TextChanged(object sender, TextChangedEventArgs e)
    {
        _logFilter = LogFilter.Text ?? "";
        await RenderLogLinesAsync(++_logRenderGeneration);
    }

    /// <summary>Confirm then <see cref="Native.LogsClear"/>. WinUI has no destructive button
    /// style — AccentButtonStyle recolored to ERROR red (not brand accent). No DefaultButton:
    /// that fights the red override and would make Enter trigger Clear.</summary>
    private async void LogClear_Click(object sender, RoutedEventArgs e)
    {
        var danger = new SolidColorBrush(Brand.LogLevelColor("ERROR") ?? Color.FromArgb(255, 0xE8, 0x46, 0x46));
        var dialog = new ContentDialog
        {
            XamlRoot = Content.XamlRoot,
            Title = Loc.T("logs.clear_confirm_title"), // title-only — no separate body, one line
            PrimaryButtonText = Loc.T("logs.clear_confirm_action"),
            CloseButtonText = Loc.T("common.cancel"),
            PrimaryButtonStyle = (Style)Application.Current.Resources["AccentButtonStyle"],
        };
        dialog.Resources["AccentButtonBackground"] = danger;
        dialog.Resources["AccentButtonBackgroundPointerOver"] = danger;
        dialog.Resources["AccentButtonBackgroundPressed"] = danger;
        if (await dialog.ShowAsync() == ContentDialogResult.Primary)
        {
            int loadGeneration = ++_logLoadGeneration;
            ++_logRenderGeneration;
            await System.Threading.Tasks.Task.Run(Native.LogsClear);
            if (loadGeneration == _logLoadGeneration && LogTab.Visibility == Visibility.Visible)
                await LoadLogsAsync(loadGeneration);
        }
    }

    /// <summary>Filter + color-code lines; yield every 64 so large logs don't freeze input.</summary>
    private async System.Threading.Tasks.Task RenderLogLinesAsync(int renderGeneration)
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
        int rendered = 0;
        foreach (var l in shown)
        {
            if (renderGeneration != _logRenderGeneration || LogTab.Visibility != Visibility.Visible) return;
            var para = new Paragraph { Margin = new Thickness(0) };
            para.Inlines.Add(new Run { Text = l.Source, Foreground = SourceBrush(l.Source), FontWeight = FontWeights.SemiBold });
            para.Inlines.Add(new Run { Text = "  " });
            var msgBrush = LevelBrush(l.Level);
            if (l.Level.Length > 0 && l.Level != "INFO")
                para.Inlines.Add(new Run { Text = l.Level + " ", Foreground = msgBrush ?? Gray });
            var msg = new Run { Text = l.Text };
            if (msgBrush != null) msg.Foreground = msgBrush;
            para.Inlines.Add(msg);
            LogText.Blocks.Add(para);
            if (++rendered % 64 == 0) await System.Threading.Tasks.Task.Yield();
        }
        if (renderGeneration != _logRenderGeneration || LogTab.Visibility != Visibility.Visible) return;
        DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low,
            () => LogScroll?.ChangeView(null, LogScroll.ScrollableHeight, null, true));
    }

    private SolidColorBrush? LevelBrush(string level)
    {
        if (level.Length == 0) return null;
        if (_levelBrushCache.TryGetValue(level, out var b)) return b;
        if (Brand.LogLevelColor(level) is not Color c) return null;
        var brush = new SolidColorBrush(c);
        _levelBrushCache[level] = brush;
        return brush;
    }

    // First-appearance index into Brand.LogSourcePalette — same mapping every platform.
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

    private bool _refreshing;

    /// <summary>Bounded off-UI probe (2500ms). Skip while hidden; never latch _refreshing on hang.</summary>
    private async void RefreshStatus()
    {
        if (!AppWindow.IsVisible) return;
        if (_refreshing) return;
        _refreshing = true;
        HealthSnapshot? snap = null;
        try
        {
            var probe = System.Threading.Tasks.Task.Run(HealthSnapshot.Probe);
            // Cap so a stuck read can't keep _refreshing latched forever.
            var done = await System.Threading.Tasks.Task.WhenAny(
                probe, System.Threading.Tasks.Task.Delay(2500));
            if (done == probe) snap = await probe;
        }
        catch { /* retry next cycle */ }
        finally { _refreshing = false; }

        if (snap is null) return;
        try { ApplyStatus(snap); } catch { /* one bad frame must not kill the loop */ }
    }

    /// <summary>Push from App's WaitModelStatus thread (already on UI). No-op while hidden.</summary>
    internal void ApplyPushed(HealthSnapshot s)
    {
        if (!AppWindow.IsVisible) return;
        try { ApplyStatus(s); } catch { /* one bad frame must not kill the push */ }
    }

    private void ApplyStatus(HealthSnapshot s)
    {
        EngineDot.Fill = s.Activity.EngineRunning ? Green : Gray;
        TtsAllTime.Text = Native.DurationLive(s.Lifetime.TtsSecs);
        SttAllTime.Text = Native.DurationLive(s.Lifetime.SttSecs);
        var v = Native.Version();
        VersionText.Text = v.Length > 0 ? v : Loc.T("common.dash");

        if (s.EngineSelection.TtsEngine == "off")
        { TtsDetail.Text = ""; ApplyOff(TtsDot, TtsRing); }
        else if (s.EngineSelection.TtsEngine == "system")
        { TtsDetail.Text = Loc.T("status.engine.system"); ApplyEngine(s.EngineDots.TtsSystem, TtsDot, TtsRing); }
        else
        { TtsDetail.Text = Loc.T("status.engine.kokoro"); ApplyEngine(s.EngineDots.Kokoro, TtsDot, TtsRing); }

        switch (s.EngineSelection.SttEngine)
        {
            case "off":
                SttDetail.Text = "";
                ApplyOff(SttDot, SttRing); break;
            case "claude_code":
                SttDetail.Text = Loc.T("status.engine.claude_code");
                ApplyEngine(s.EngineDots.ClaudeCode, SttDot, SttRing); break;
            case "system":
                SttDetail.Text = Loc.T("status.engine.system");
                ApplyEngine(s.EngineDots.System, SttDot, SttRing); break;
            default:
                SttDetail.Text = Loc.T("status.engine.parakeet");
                ApplyEngine(s.EngineDots.Parakeet, SttDot, SttRing); break;
        }

        // Shared formatter returns "" for ready states — emptiness is note-vs-stats (all platforms).
        // Runtime line only when ready (never stale "ORT CPU" under Downloading N%).
        bool ttsSystem = s.EngineSelection.TtsEngine == "system";
        var ttsInfo = s.ActiveTts;
        bool ttsTrouble = !string.IsNullOrEmpty(ttsInfo.Word);
        TtsRuntimeRow.Visibility = (!ttsSystem && !ttsTrouble && s.EngineSelection.TtsProvider.Length > 0) ? Visibility.Visible : Visibility.Collapsed;
        if (!ttsSystem) TtsRuntimeText.Text = Native.RuntimeLabel(s.EngineSelection.TtsProvider);
        TtsSystemSettingsRow.Visibility = Visibility.Collapsed;
        if (ttsTrouble)
            ShowMsg(TtsStatsMsg, TtsStatsGrid, ttsInfo.Word);
        else if (ttsSystem)
            ShowSystemVoiceLink();
        else if (s.Tts.Utterances == 0)
            ShowMsg(TtsStatsMsg, TtsStatsGrid, Loc.T("status.no_data"));
        else
        {
            ShowGrid(TtsStatsMsg, TtsStatsGrid);
            TtsSpeed.Text = Native.StatsRange(s.Tts.RtfMin, s.Tts.RtfAvg, s.Tts.RtfMax, 2, "status.stats.unit.times");
            TtsFirst.Text = Native.StatsRange(s.Tts.FirstMinMs / 1000, s.Tts.FirstAvgMs / 1000, s.Tts.FirstMaxMs / 1000, 1, "status.stats.unit.seconds");
            TtsSpoken.Text = Native.StatsCount((ulong)s.Tts.Utterances, s.Tts.AudioSecs);
            TtsFailuresRow.Visibility = s.Tts.Failures > 0 ? Visibility.Visible : Visibility.Collapsed;
            if (s.Tts.Failures > 0)
                TtsFailures.Text = s.Tts.Failures.ToString(System.Globalization.CultureInfo.InvariantCulture);
        }

        bool sttBuiltIn = s.EngineSelection.SttEngine == "built_in";
        var sttInfo = s.ActiveStt;
        bool sttTrouble = !string.IsNullOrEmpty(sttInfo.Word);
        SttRuntimeRow.Visibility = (sttBuiltIn && !sttTrouble && s.EngineSelection.SttProvider.Length > 0) ? Visibility.Visible : Visibility.Collapsed;
        if (sttBuiltIn) SttRuntimeText.Text = Native.RuntimeLabel(s.EngineSelection.SttProvider);
        if (sttTrouble)
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
            SttFailuresRow.Visibility = s.Stt.Failures > 0 ? Visibility.Visible : Visibility.Collapsed;
            if (s.Stt.Failures > 0)
                SttFailures.Text = s.Stt.Failures.ToString(System.Globalization.CultureInfo.InvariantCulture);
        }

        // CS0162: const false makes body unreachable; suppress so the flip remains a const bool.
#pragma warning disable CS0162 // Unreachable code detected
        if (DiarizationUiEnabled)
        {
            var diarInfo = s.EngineDots.Diarization;
            bool diarTrouble = !string.IsNullOrEmpty(diarInfo.Word);
            ApplyEngine(diarInfo, DiarDot, DiarRing);
            if (diarTrouble)
                ShowMsg(DiarStatsMsg, DiarStatsGrid, diarInfo.Word);
            else if (!s.Diarization.Enabled)
                ShowMsg(DiarStatsMsg, DiarStatsGrid, Loc.T("status.diarization_disabled"));
            else if (s.Diarization.Speakers.Length == 0)
                ShowMsg(DiarStatsMsg, DiarStatsGrid, Loc.T("status.diarization_no_speakers"));
            else
            {
                ShowGrid(DiarStatsMsg, DiarStatsGrid);
                DiarRuntimeRow.Visibility = s.Diarization.Runtime.Length > 0 ? Visibility.Visible : Visibility.Collapsed;
                if (s.Diarization.Runtime.Length > 0) DiarRuntimeText.Text = Native.RuntimeLabel(s.Diarization.Runtime);
                DiarEnrolled.Text = string.Join(", ", s.Diarization.Speakers);
                DiarSensitivity.Text = s.Diarization.ClusteringThreshold.ToString("F2", System.Globalization.CultureInfo.InvariantCulture);
            }
        }
#pragma warning restore CS0162

        ApplyStateAccent(s.IndicatorState());
        CapsDot.Fill = s.Activity.Caps ? Green : Gray;
    }

    private TrayIcon.IconState _accentState = (TrayIcon.IconState)(-1);

    private void SizeStateStripe()
    {
        double h = 48; // WinUI top-nav fallback
        if (FindDescendant(Nav, "TopNavGrid") is FrameworkElement bar && bar.ActualHeight > 0)
            h = bar.ActualHeight;
        StateStripe.Height = h;
    }

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

    /// <summary>Top bar wash in tray Brand tints; idle clears. ~30% so tabs stay readable on Mica.</summary>
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
        if (tint is not Windows.UI.Color basis)
        {
            StateStripe.Background = null;
            return;
        }
        const double Tint = 0.30;
        StateStripe.Background = new Microsoft.UI.Xaml.Media.SolidColorBrush(
            Windows.UI.Color.FromArgb((byte)(255 * Tint), basis.R, basis.G, basis.B));
    }

    /// <summary>Dot color only (trouble note lives in expansion). Download → orange progress ring
    /// with 0.02 floor so 0% still shows a sliver (macOS parity).</summary>
    private static void ApplyEngine(EngineInfo e, Microsoft.UI.Xaml.Shapes.Ellipse dot,
                                    Microsoft.UI.Xaml.Controls.ProgressRing ring)
    {
        if (e.State == EngineState.Downloading)
        {
            ring.Value = Math.Clamp(e.Progress, 0.02, 1.0);
            ring.Visibility = Visibility.Visible;
            dot.Visibility = Visibility.Collapsed;
            return;
        }
        ring.Visibility = Visibility.Collapsed;
        dot.Visibility = Visibility.Visible;
        dot.Fill = e.State switch
        {
            EngineState.Running => Green,
            EngineState.Warming => Orange,
            EngineState.Blocked => Orange,
            EngineState.Failed => Red,
            _ => Gray,
        };
    }

    private static void ApplyOff(Microsoft.UI.Xaml.Shapes.Ellipse dot,
                                 Microsoft.UI.Xaml.Controls.ProgressRing ring)
    {
        ring.Visibility = Visibility.Collapsed;
        dot.Visibility = Visibility.Visible;
        dot.Fill = Gray;
    }

    /// <summary>Claude Code ready row names the delegated key instead of local STT stats.</summary>
    private static string ClaudeDelegationHint(HealthSnapshot s) =>
        s.EngineSelection.ClaudeCodeKey.Length > 0
            ? Loc.T("status.stt_claude_code", new Dictionary<string, string> { ["key"] = s.EngineSelection.ClaudeCodeKey })
            : Loc.T("status.stt_claude_code_off");

    private static void ShowMsg(TextBlock msg, FrameworkElement grid, string text)
    {
        msg.Text = text; msg.Visibility = Visibility.Visible; grid.Visibility = Visibility.Collapsed;
    }
    private static void ShowGrid(TextBlock msg, FrameworkElement grid)
    {
        msg.Visibility = Visibility.Collapsed; grid.Visibility = Visibility.Visible;
    }
    private void ShowSystemVoiceLink()
    {
        TtsSystemSettingsText.Text = Loc.T("status.tts_system_settings");
        TtsStatsMsg.Visibility = Visibility.Collapsed;
        TtsStatsGrid.Visibility = Visibility.Collapsed;
        TtsSystemSettingsRow.Visibility = Visibility.Visible;
    }

    private async void VersionLink_Click(object sender, RoutedEventArgs e)
    {
        var url = Native.HomepageUrl();
        if (url.Length > 0 && Uri.TryCreate(url, UriKind.Absolute, out var uri))
            await Windows.System.Launcher.LaunchUriAsync(uri);
    }

    /// <summary>Startup one-shot pill (latestVersion null when available is false). Does not
    /// change VersionLink (still opens homepage). SeedPurple wash only — not error/warning.</summary>
    internal void ApplyUpdateCheck(bool available, string? latestVersion)
    {
        if (!available || latestVersion is null) return;
        UpdateBadgeText.Text = latestVersion;
        UpdateArrowText.Visibility = UpdateBadgeText.Visibility = Visibility.Visible;
        var purple = Brand.SeedPurple;
        VersionPill.Background = new SolidColorBrush(Color.FromArgb(40, purple.R, purple.G, purple.B));
    }

    // HyperlinkButton.Click doesn't mark Tapped Handled — without this, bubbles to header expand.
    private void VersionLink_Tapped(object sender, Microsoft.UI.Xaml.Input.TappedRoutedEventArgs e) => e.Handled = true;

    private void TtsSystemSettings_Click(object sender, RoutedEventArgs e) => Native.OpenVoiceSettings();

    private void DontSpeakHeader_Tapped(object sender, Microsoft.UI.Xaml.Input.TappedRoutedEventArgs e) => ToggleStats(DontSpeakStats);
    private void TtsHeader_Tapped(object sender, Microsoft.UI.Xaml.Input.TappedRoutedEventArgs e) => ToggleStats(TtsStats);
    private void SttHeader_Tapped(object sender, Microsoft.UI.Xaml.Input.TappedRoutedEventArgs e) => ToggleStats(SttStats);
    private void DiarHeader_Tapped(object sender, Microsoft.UI.Xaml.Input.TappedRoutedEventArgs e) => ToggleStats(DiarStats);
    private void CapsHeader_Tapped(object sender, Microsoft.UI.Xaml.Input.TappedRoutedEventArgs e) => ToggleStats(CapsStats);
    private static void ToggleStats(FrameworkElement panel)
    {
        panel.Visibility = panel.Visibility == Visibility.Visible ? Visibility.Collapsed : Visibility.Visible;
    }

    // Tab strip + Status panel top/bottom margins → client height chrome (bottom == side pad).
    private const double StatusChromeDip = 84;

    // Last auto-fit client height (-1 = never). Match ⇒ still tracking content; else honor user taller size.
    private int _lastFitClientPx = -1;

    /// <summary>Min height = Status content (no shorter cut-off); no max. Auto-fit unless user
    /// dragged taller. Use arranged ActualHeight — manual Measure blanks the window.</summary>
    private void CapHeightToStatusContent()
    {
        if (AppWindow.Presenter is not Microsoft.UI.Windowing.OverlappedPresenter pr) return;
        if (StatusScroll?.Content is not FrameworkElement panel || Content?.XamlRoot is null) return;
        double scale = Content.XamlRoot.RasterizationScale;
        if (scale <= 0 || panel.ActualHeight <= 0) return;
        int clientPx = (int)Math.Ceiling((panel.ActualHeight + StatusChromeDip) * scale);
        int nonClientPx = Math.Max(0, AppWindow.Size.Height - AppWindow.ClientSize.Height);
        pr.PreferredMinimumHeight = clientPx + nonClientPx;
        pr.PreferredMaximumHeight = null;
        // Height-only resize doesn't fire panel SizeChanged — no loop.
        int cur = AppWindow.ClientSize.Height;
        bool atAutoFit = _lastFitClientPx < 0 || Math.Abs(cur - _lastFitClientPx) <= 2;
        if (atAutoFit || cur < clientPx)
        {
            if (Math.Abs(cur - clientPx) > 2)
                AppWindow.ResizeClient(new Windows.Graphics.SizeInt32(AppWindow.ClientSize.Width, clientPx));
            _lastFitClientPx = clientPx;
        }
    }

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

            // Fluent Expander per tool; catalog order is authored display order (macOS same source).
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
                    head.Children.Add(new TextBlock { Text = string.IsNullOrEmpty(p.Type) ? Loc.T("tools.param.type_any") : p.Type, FontSize = 12, Opacity = 0.6, VerticalAlignment = VerticalAlignment.Center });
                    var req = new TextBlock { Text = p.Required ? Loc.T("tools.param.required") : Loc.T("tools.param.optional"), FontSize = 12, VerticalAlignment = VerticalAlignment.Center };
                    if (p.Required) req.Foreground = Orange; else req.Opacity = 0.6;
                    head.Children.Add(req);
                    var detail = p.Detail ?? "";
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

    /// <summary>Libraries from shared ds-model catalog — credits can't drift from what ships.</summary>
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

            var links = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 16 };
            if (!string.IsNullOrEmpty(p.Homepage) && Uri.TryCreate(p.Homepage, UriKind.Absolute, out var hp))
                links.Children.Add(new HyperlinkButton { Content = Loc.T("libraries.homepage"), NavigateUri = hp, Padding = new Thickness(0), MinWidth = 0, MinHeight = 0 });
            var lic = p.License ?? "";
            if (lic.Length > 0 && !string.IsNullOrEmpty(p.LicenseUrl) && Uri.TryCreate(p.LicenseUrl, UriKind.Absolute, out var lu))
                links.Children.Add(new HyperlinkButton { Content = lic, NavigateUri = lu, Padding = new Thickness(0), MinWidth = 0, MinHeight = 0 });
            if (links.Children.Count > 0) body.Children.Add(links);

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
                        var sizeTb = new TextBlock { Text = Native.HumanSize((ulong)sz), FontSize = 12, Opacity = 0.5, HorizontalAlignment = HorizontalAlignment.Right, VerticalAlignment = VerticalAlignment.Center, Margin = new Thickness(8, 0, 0, 0) };
                        Grid.SetColumn(sizeTb, 1);
                        row.Children.Add(sizeTb);
                    }
                    body.Children.Add(row);
                }
            }

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

    // Wire: ds-model libraries::catalog (ordered projects/files).
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

    // Wire: ds-tools catalog_ui (ordered tools/params); macOS ToolDTO parity.
    private sealed record ToolDto(
        [property: JsonPropertyName("name")] string? Name,
        [property: JsonPropertyName("description")] string? Description,
        [property: JsonPropertyName("params")] List<ToolParamDto>? Params);

    private sealed record ToolParamDto(
        [property: JsonPropertyName("name")] string? Name,
        [property: JsonPropertyName("type")] string? Type,
        [property: JsonPropertyName("required")] bool Required,
        [property: JsonPropertyName("description")] string? Description,
        // Pre-built by status_fmt::tool_param_detail — no host-side derivation.
        [property: JsonPropertyName("detail")] string? Detail);
}
