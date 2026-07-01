using System;
using System.Linq;
using System.Threading;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;

namespace DontSpeak;

/// <summary>
/// The DontSpeak app entry point — the Windows analogue of the macOS SwiftUI app.
/// This ONE process is both the resident host and the Fluent UI (Option A: the
/// old Rust ds-tray was merged in here):
///   • HOSTS the engine IN-PROCESS via <see cref="Native.EngineStart"/> on launch
///     / <see cref="Native.EngineStop"/> on quit (caps loop + RPC server + TTS
///     queue on a Rust background thread inside this process, over ds_core.dll).
///   • shows a state-colored tray icon + menu (<see cref="TrayIcon"/>), and the
///     Status/Tools window. Closing the window HIDES it to the tray; the engine
///     keeps running. Exit is the tray's Exit item.
///   • leaves ALL runtime control (voice/engine/rate/toggles) to DontSpeak.
/// </summary>
public partial class App : Application
{
    private MainWindow? _window;
    private TrayIcon? _tray;
    private DictationPanel? _panel;
    private bool _exiting;
    private bool _hostingEngine;
    private int _promoteTries;
    private bool _hintedTray;
    private bool _testOverlay;            // --test-overlay: cycle the dictation panel for visual QA
    private DispatcherQueueTimer? _testTimer;
    private Thread? _pushThread;          // dedicated thread blocking in WaitModelStatus (overlay push)
    private volatile bool _pushStop;
    private static Mutex? _instanceMutex;
    private static EventWaitHandle? _activate;
    private const string ActivateEvent = "DontSpeak.WinUI.Activate";
    // The app's explicit AppUserModelID. The installer stamps this SAME id on the Start-menu
    // shortcut (dontspeak.iss [Icons] AppUserModelID) whose name is "DontSpeak", so Windows
    // resolves this id to that name — making the taskbar + Task Manager "Apps" group read
    // "DontSpeak" instead of the "ds-winui" exe-name fallback. Keep the two in sync.
    private const string AppUserModelId = "DontSpeak";

    public App()
    {
        // Claim our app identity FIRST — before any window/tray/UI — so the taskbar and Task
        // Manager group this process as "DontSpeak" (see AppUserModelId). Best-effort.
        try { Win32.SetCurrentProcessExplicitAppUserModelID(AppUserModelId); } catch { }
        EnablePortableModelDir();
        InitializeComponent();
    }

    /// <summary>Portable build: if a `models` folder sits next to the .exe (the extracted
    /// bundle ships one) and DONTSPEAK_MODEL_DIR isn't already set, point the engine at it so an
    /// extracted, NO-INSTALL copy uses its BUNDLED models in place. Set on the process env BEFORE
    /// the engine DLL is P/Invoked, so the in-process engine + any child it spawns (helper,
    /// dontspeak.exe) inherit it. The installed app has no sibling `models` dir, so it falls
    /// through to the per-user cache (model_dir) as before.</summary>
    private static void EnablePortableModelDir()
    {
        if (!string.IsNullOrEmpty(Environment.GetEnvironmentVariable("DONTSPEAK_MODEL_DIR"))) return;
        try
        {
            var models = System.IO.Path.Combine(AppContext.BaseDirectory, "models");
            if (System.IO.Directory.Exists(models))
                Environment.SetEnvironmentVariable("DONTSPEAK_MODEL_DIR", models);
        }
        catch { /* best-effort; fall back to the per-user cache */ }
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        // Single instance: a second launch (e.g. clicking the app while it's
        // resident in the tray) just exits and leaves the running one.
        _instanceMutex = new Mutex(true, "DontSpeak.WinUI.SingleInstance", out bool createdNew);
        if (!createdNew)
        {
            // Already running: signal that instance to surface its window, then exit — this
            // is how a re-launch (Start menu / desktop icon) reopens a window that was closed
            // to the tray, instead of the new process exiting and the old one staying hidden.
            if (EventWaitHandle.TryOpenExisting(ActivateEvent, out var ev)) { ev.Set(); ev.Dispose(); }
            Exit(); return;
        }
        _activate = new EventWaitHandle(false, EventResetMode.AutoReset, ActivateEvent);

        var cli = Environment.GetCommandLineArgs();
        bool Has(params string[] flags) =>
            cli.Any(a => flags.Any(f => a.Equals(f, StringComparison.OrdinalIgnoreCase)));
        // "--hidden"/"--tray" (the autostart form) starts resident in the tray with
        // no window; "tools"/"--tools" selects the Tools tab on first show.
        bool hidden = Has("--hidden", "--tray");
        bool tools = Has("tools", "--tools");
        bool testGlow = Has("--test-glow");   // hold the empty "listening" glow steady for QA
        _testOverlay = Has("--test-overlay") || testGlow; // visual QA: drive the panel directly

        // Be the resident engine host — but only if nothing already answers the
        // socket (the MCP launches this host app when no engine is up). This
        // keeps us a polite client instead of double-binding the socket; we only
        // stop on exit what we ourselves started.
        _hostingEngine = !HealthSnapshot.Probe().Activity.EngineRunning;
        if (_hostingEngine) Native.EngineStart();

        _tray = new TrayIcon();
        _tray.OpenStatus += () => ShowWindow(false);
        _tray.Exit += ExitApp;

        // The dictation transcript overlay (the macOS OverlayPanel analogue), driven from the
        // same status push as the tray. Its glow tints to a warning color when no editable
        // target is focused.
        _panel = new DictationPanel();

        _window = new MainWindow();
        // Close → hide to tray (stay resident); real teardown is the tray's Exit.
        _window.AppWindow.Closing += (_, e) =>
        {
            if (_exiting) return;
            e.Cancel = true;
            _window!.AppWindow.Hide();
            // First time it hides, tell the user where it went + how to get back —
            // the Win11 tray hides new icons in the ^ overflow.
            if (!_hintedTray)
            {
                _hintedTray = true;
                _tray?.Balloon(Loc.T("tray.hint_tray_title"), Loc.T("tray.hint_tray_body"));
            }
        };

        if (!hidden) ShowWindow(tools);
        else _tray.Balloon(Loc.T("tray.hint_tray_title"), Loc.T("tray.hint_tray_body"));

        var q = DispatcherQueue.GetForCurrentThread();
        // Paint the tray immediately (synchronous one-shot) so the icon/tooltip show before the
        // push thread delivers its first change — the first ModelStatusWait(0,…) can block up to
        // its 1s timeout. After this the push thread (StartDictationPush) is the SOLE driver of
        // the tray, stats, AND dictation overlay; there is no poll timer (Windows has no
        // OS-permission polling to do, so a push gate bump on every status change covers it).
        ApplyStatus(HealthSnapshot.Probe());

        // Low-latency status PUSH: a dedicated thread blocks in the engine's WaitModelStatus and
        // re-renders the instant the engine bumps its status gate (it now bumps on EVERY status
        // change, not just dictation). A DEDICATED thread (not the thread pool — the call blocks
        // indefinitely; pooling it risks starvation). It is the SOLE driver of the tray, stats,
        // and dictation overlay. Skipped in the visual-QA overlay modes (which drive the panel
        // directly).
        if (!_testOverlay) StartDictationPush();

        // --test-overlay: cycle the dictation panel without a live Parakeet dictation. The push
        // thread is skipped (above), so nothing fights the scripted panel updates.
        if (testGlow)
        {
            // Hold the empty "listening" state so the breathing glow can be inspected
            // (empty card ⇒ speak-now glow, mirroring the engine's prompt_glow).
            _panel?.Update(true, "", true, true);
        }
        else if (_testOverlay)
        {
            // Simulate a streaming dictation: listening glow → words fade in one at a time →
            // the last word is REPLACED (re-fades), then loops. Eyeballs the per-word fade +
            // word-replacement animation.
            string[] script =
            {
                "",                                       // listening (breathing glow)
                "Accurate",
                "Accurate speech",
                "Accurate speech recognition",
                "Accurate speech recognition requires",
                "Accurate speech recognition requires powerful",
                "Accurate speech recognition requires powerful processing",
                // Backtracks: LONG words REPLACED at a fixed slot — the blur-replace cross-fade.
                "Accurate speech recognizing requires powerful processing",   // recognition → recognizing
                "Accurate speech recognizing demands powerful processing",    // requires → demands
                "Accurate speech recognizing demands powerful processors",    // processing → processors
                "Approximate speech recognizing demands powerful processors", // Accurate → Approximate (leading slot)
            };
            int i = 0;
            // Simulated speak-now glow: pulse only on the empty (listening) step (empty ⇒
            // glow, words ⇒ static), matching the engine's prompt_glow.
            _panel?.Update(true, script[0], true, string.IsNullOrWhiteSpace(script[0]));
            _testTimer = q.CreateTimer();
            _testTimer.Interval = TimeSpan.FromMilliseconds(1000); // > the fade, so each step's transition is distinct
            _testTimer.Tick += (_, _) =>
            {
                i = (i + 1) % script.Length;
                _panel?.Update(true, script[i], true, string.IsNullOrWhiteSpace(script[i]));
            };
            _testTimer.Start();
        }

        // Surface the window whenever another launch signals us (single-instance
        // reactivation) — so re-running from the Start menu reopens the closed window.
        var uiq = DispatcherQueue.GetForCurrentThread();
        new Thread(() =>
        {
            // Wakes on a second-launch signal (Set) and on exit (ExitApp sets _exiting +
            // Set). Terminates on _exiting; the try/catch covers a Set-then-Dispose race.
            try
            {
                while (!_exiting)
                {
                    _activate!.WaitOne();
                    if (_exiting) break;
                    uiq.TryEnqueue(() => { if (!_exiting) ShowWindow(false); });
                }
            }
            catch { /* handle disposed during teardown */ }
        }) { IsBackground = true }.Start();
    }

    private void ShowWindow(bool tools)
    {
        if (_window == null) return;
        _window.SelectTab(tools);
        _window.AppWindow.Show();   // un-hide from the tray
        var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(_window);
        Win32.ShowWindow(hwnd, SW_RESTORE);   // restore if minimized
        // Bring it to the front reliably even when SetForegroundWindow is restricted — which it is
        // from the tray RIGHT-click menu (the system blocks SetForegroundWindow "while a menu is
        // active"). The documented workaround: make the window topmost (this activates it), call
        // SetForegroundWindow while it's topmost, then drop topmost — it stays frontmost in the
        // normal Z-order band. The topmost move doesn't depend on foreground rights, so it sticks.
        SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
        SetForegroundWindow(hwnd);
        SetWindowPos(hwnd, HWND_NOTOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
    }

    private const int SW_RESTORE = 9;
    private static readonly IntPtr HWND_TOPMOST = new(-1), HWND_NOTOPMOST = new(-2);
    private const uint SWP_NOSIZE = 0x0001, SWP_NOMOVE = 0x0002;
    [System.Runtime.InteropServices.DllImport("user32.dll")] private static extern bool SetForegroundWindow(IntPtr hWnd);
    [System.Runtime.InteropServices.DllImport("user32.dll", SetLastError = true)]
    private static extern bool SetWindowPos(IntPtr hWnd, IntPtr insertAfter, int x, int y, int cx, int cy, uint flags);

    /// <summary>Paint the tray (icon + tooltip) from an already-probed status snapshot. Runs on
    /// the UI thread and is synchronous — the caller supplies the snapshot (the push thread reads
    /// it OFF the UI thread; the startup/visible one-shot probes inline). One definition so the
    /// push callback and the initial paint can't diverge.</summary>
    private void ApplyStatus(HealthSnapshot s)
    {
        // Pin the icon onto the taskbar (out of the Win11 overflow). The shell creates
        // the NotifyIconSettings entry a beat after NIM_ADD, so retry on the first dozen
        // status pushes until PromoteInTray finds + promotes it.
        if (_promoteTries < 12 && _tray != null && _tray.PromoteInTray())
            _promoteTries = 12;
        else
            _promoteTries++;

        var state = s.IndicatorState();   // shared tray/window indicator mapping
        _tray?.Update(state, s.Activity.Muted);
    }

    /// <summary>Spin the dedicated status-push thread: block in the engine's WaitModelStatus,
    /// then marshal each fresh snapshot onto the UI thread (TryEnqueue) to repaint the tray,
    /// stats, and dictation overlay together. The engine bumps its status sequence on EVERY
    /// status change (not just dictation), so this returns within a tick of any change — a
    /// ~0-jitter push, and the SOLE driver of the UI.</summary>
    private void StartDictationPush()
    {
        _pushThread = new Thread(() =>
        {
            ulong since = 0;   // 0 ⇒ the first call returns the current state immediately
            bool delivered = false;
            while (!_pushStop)
            {
                string json;
                try { json = Native.ModelStatusWait(since, 1000); }
                catch { Thread.Sleep(500); continue; }   // dll/engine hiccup — back off, retry
                if (_pushStop) break;
                if (string.IsNullOrWhiteSpace(json) || json == "{}")
                {
                    // Engine down or between hosts: the wait can't block, so pace ourselves
                    // to avoid a hot spin until it comes back.
                    Thread.Sleep(400);
                    continue;
                }
                var s = HealthSnapshot.FromJson(json);
                // WaitModelStatus returns on its 1s TIMEOUT with the SAME seq when nothing
                // changed (status.rs StatusGate::wait_changed). Re-marshalling that identical
                // snapshot would repaint the tray + status window + overlay on the UI thread
                // ~1×/s forever while idle. Marshal only when the sequence actually advanced (or
                // the first sample). Engine down/up surfaces here as an empty/"{}" payload,
                // caught by the early `continue` above (liveness on Windows IS payload
                // presence), so this seq-only gate is correct. NOTE: macOS deliberately
                // DIFFERS — there `engineRunning` is an external pidfile/launchd probe NOT
                // carried in the seq, so its guard ALSO yields on a `running` flip
                // (Core.startStatusProducer / statusShouldYield). Do not "unify" these by
                // dropping that clause on macOS or adding it here — the liveness models are
                // intentionally different per platform.
                bool changed = !delivered || s.StatusSeq != since;
                since = s.StatusSeq;
                if (!changed) continue;
                delivered = true;
                // Overlay gate: engine running + a local-STT (Parakeet) dictation recording or
                // awaiting the confirm tap (mirrors the macOS panel gate), off the JSON we got.
                bool showPanel = s.Activity.EngineRunning && (s.Dictation.DictAwaitingConfirm || (s.Activity.Recording && s.Dictation.DictLocalStt));
                // Render the WHOLE status on the UI thread off this one pushed snapshot — the tray
                // tooltip/icon AND the dictation overlay. Both the tray update and DictationPanel's
                // layered-window update must run on the dispatcher thread.
                _window?.DispatcherQueue.TryEnqueue(() =>
                {
                    ApplyStatus(s);                 // tray icon + tooltip
                    _window?.ApplyPushed(s);        // the status window (no-op while hidden)
                    if (!_testOverlay) _panel?.Update(showPanel, s.Dictation.DictText, s.Dictation.DictHasTarget, s.Dictation.DictPromptGlow);
                });
            }
        })
        { IsBackground = true, Name = "dictation-push" };
        _pushThread.Start();
    }

    private void ExitApp()
    {
        if (_exiting) return;
        _exiting = true;
        _pushStop = true;   // the push thread is background + wakes within its 1s wait cap
        _activate?.Set();   // wake the reactivation thread so it observes _exiting and ends
        _panel?.Dispose();
        _tray?.Dispose();
        if (_hostingEngine) Native.EngineStop();
        _window?.Close();
        Exit();
    }
}
