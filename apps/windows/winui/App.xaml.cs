using System;
using System.Globalization;
using System.Linq;
using System.Runtime.InteropServices;
using System.Threading;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;

namespace DontSpeak;

/// <summary>
/// Process entry: in-process engine host + Fluent UI.
/// <see cref="Native.EngineStart"/> on launch / <see cref="Native.EngineStop"/> on quit;
/// tray + Status/Tools window. Close hides to tray; Exit is tray-only. Runtime control via MCP.
/// </summary>
[System.Diagnostics.CodeAnalysis.SuppressMessage("Design", "CA1001:Types that own disposable fields should be disposable",
    Justification = "App lives for the whole process; the tray/panel are torn down by ExitApp, not a Dispose call")]
public partial class App : Application
{
    private MainWindow? _window;
    private TrayIcon? _tray;
    private DictationPanel? _panel;
    private bool _exiting;
    private bool _hostingEngine;
    private int _promoteTries;
    private bool _hintedTray;
    private bool _testOverlay; // --test-overlay visual QA
    private DispatcherQueueTimer? _testTimer;
    private Thread? _pushThread; // blocks in WaitModelStatus
    private volatile bool _pushStop;
    private readonly object _statusDispatchLock = new();
    private HealthSnapshot? _pendingStatus;
    private bool _statusDispatchQueued;
    private static Mutex? _instanceMutex;
    private static EventWaitHandle? _activate;
    private const string ActivateEvent = "DontSpeak.WinUI.Activate";
    // Pairs with Start-menu shortcut (install.ps1); set before any UI. See Win32.AUMID note.
    private const string AppUserModelId = "DontSpeak";

    public App()
    {
        try { _ = Win32.SetCurrentProcessExplicitAppUserModelID(AppUserModelId); } catch { }
        EnablePortableModelDir();
        InitializeComponent();
    }

    /// <summary>Portable: sibling `models/` + unset DONTSPEAK_MODEL_DIR → set env before any
    /// P/Invoke so engine + children inherit. Else per-user cache.</summary>
    private static void EnablePortableModelDir()
    {
        if (!string.IsNullOrEmpty(Environment.GetEnvironmentVariable("DONTSPEAK_MODEL_DIR"))) return;
        try
        {
            var models = System.IO.Path.Combine(AppContext.BaseDirectory, "models");
            if (System.IO.Directory.Exists(models))
                Environment.SetEnvironmentVariable("DONTSPEAK_MODEL_DIR", models);
        }
        catch { /* best-effort */ }
    }

    /// <summary>2500ms cap (same as RefreshStatus) — ModelStatusJson can block ~120s if wedged.</summary>
    private static HealthSnapshot ProbeBounded()
    {
        var probe = System.Threading.Tasks.Task.Run(HealthSnapshot.Probe);
        return probe.Wait(2500) ? probe.Result : new HealthSnapshot();
    }

    /// <summary>
    /// Only user-facing string outside <see cref="Loc.T(string)"/>: Loc.T P/Invokes ds_core,
    /// and this path only runs when that DLL is unloadable.
    /// </summary>
    private static string DllLoadFailureMessage() =>
        CultureInfo.CurrentUICulture.TwoLetterISOLanguageName switch
        {
            // ds_core unreachable — English only.
            _ => "ds_core.dll was not found next to the app, so the voice engine cannot start.\n\n" +
                 "Reinstall DontSpeak (irm https://github.com/delllusional/DontSpeak/releases/latest/download/install.ps1 | iex). Building from " +
                 "source? Build the Rust engine first — see apps/windows/installer/build-portable.ps1.",
        };

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        // Fail with a window if DLL missing (partial install / C# without Rust build).
        // No Loc.T — see DllLoadFailureMessage.
        if (!NativeLibrary.TryLoad("ds_core.dll", out _))
        {
            _ = Win32.MessageBoxW(IntPtr.Zero, DllLoadFailureMessage(),
                "DontSpeak", Win32.MB_OK | Win32.MB_ICONERROR);
            Exit(); return;
        }

        // Second launch signals first to show window, then exits.
        _instanceMutex = new Mutex(true, "DontSpeak.WinUI.SingleInstance", out bool createdNew);
        if (!createdNew)
        {
            if (EventWaitHandle.TryOpenExisting(ActivateEvent, out var ev)) { ev.Set(); ev.Dispose(); }
            Exit(); return;
        }
        _activate = new EventWaitHandle(false, EventResetMode.AutoReset, ActivateEvent);

        var cli = Environment.GetCommandLineArgs();
        bool Has(params string[] flags) =>
            cli.Any(a => flags.Any(f => a.Equals(f, StringComparison.OrdinalIgnoreCase)));
        // --hidden/--tray: autostart tray-only; tools/--tools: Tools tab on first show.
        bool hidden = Has("--hidden", "--tray");
        bool tools = Has("tools", "--tools");
        bool testGlow = Has("--test-glow");
        _testOverlay = Has("--test-overlay") || testGlow;

        // Start engine only if socket free; stop on exit only what we started.
        _hostingEngine = !ProbeBounded().Activity.EngineRunning;
        if (_hostingEngine) Native.EngineStart();

        _tray = new TrayIcon();
        // Re-show keeps the selected tab (first open uses XAML default).
        _tray.OpenStatus += () => ShowWindow();
        _tray.Exit += ExitApp;

        _panel = new DictationPanel();

        _window = new MainWindow();
        // Close hides to tray; real teardown is tray Exit.
        _window.AppWindow.Closing += (_, e) =>
        {
            if (_exiting) return;
            e.Cancel = true;
            _window!.AppWindow.Hide();
            // Win11 often parks new icons in overflow — balloon once.
            if (!_hintedTray)
            {
                _hintedTray = true;
                _tray?.Balloon(Loc.T("tray.hint_tray_title"), Loc.T("tray.hint_tray_body"));
            }
        };

        if (!hidden) ShowWindow(tools ? "tools" : null);
        else _tray.Balloon(Loc.T("tray.hint_tray_title"), Loc.T("tray.hint_tray_body"));

        var q = DispatcherQueue.GetForCurrentThread();
        // One-shot paint before push (first Wait can block 1s); then push alone drives UI.
        ApplyStatus(ProbeBounded());

        // Dedicated thread (not pool — Wait blocks indefinitely). Skip for QA overlay.
        if (!_testOverlay) StartDictationPush();

        // GitHub GET blocks — off UI; fire-and-forget pill.
        new Thread(() =>
        {
            bool available; string? latest;
            try
            {
                var json = Native.UpdateCheckJson();
                available = Native.ParseUpdateAvailable(json);
                latest = Native.ParseLatestVersion(json);
            }
            catch { available = false; latest = null; }
            q.TryEnqueue(() => _window?.ApplyUpdateCheck(available, latest));
        })
        { IsBackground = true, Name = "update-check" }.Start();

        // Visual QA without live STT (push skipped above).
        if (testGlow)
        {
            _panel?.Update(true, "", true, true);
        }
        else if (_testOverlay)
        {
            // Listening → grow words → blur-replace backtracks.
            string[] script =
            {
                "",
                "Accurate",
                "Accurate speech",
                "Accurate speech recognition",
                "Accurate speech recognition requires",
                "Accurate speech recognition requires powerful",
                "Accurate speech recognition requires powerful processing",
                "Accurate speech recognizing requires powerful processing",
                "Accurate speech recognizing demands powerful processing",
                "Accurate speech recognizing demands powerful processors",
                "Approximate speech recognizing demands powerful processors",
            };
            int i = 0;
            _panel?.Update(true, script[0], true, string.IsNullOrWhiteSpace(script[0]));
            _testTimer = q.CreateTimer();
            _testTimer.Interval = TimeSpan.FromMilliseconds(1000); // > fade so steps are distinct
            _testTimer.Tick += (_, _) =>
            {
                i = (i + 1) % script.Length;
                _panel?.Update(true, script[i], true, string.IsNullOrWhiteSpace(script[i]));
            };
            _testTimer.Start();
        }

        // Second launch Set()s; ExitApp Set()s to end loop.
        var uiq = DispatcherQueue.GetForCurrentThread();
        new Thread(() =>
        {
            try
            {
                while (!_exiting)
                {
                    _activate!.WaitOne();
                    if (_exiting) break;
                    uiq.TryEnqueue(() => { if (!_exiting) ShowWindow(); });
                }
            }
            catch { /* disposed during teardown */ }
        }) { IsBackground = true }.Start();
    }

    private void ShowWindow(string? tab = null)
    {
        if (_window == null) return;
        if (tab != null) _window.SelectTab(tab);
        _window.AppWindow.Show();
        var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(_window);
        Win32.ShowWindow(hwnd, SW_RESTORE);
        // Tray menu blocks SetForegroundWindow; topmost → FG → drop (topmost needs no FG rights).
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

    /// <summary>UI-thread tray paint (push + startup share this).</summary>
    private void ApplyStatus(HealthSnapshot s)
    {
        // NotifyIconSettings appears after NIM_ADD — retry promote for ~12 pushes.
        if (_promoteTries < 12 && _tray != null && TrayIcon.PromoteInTray())
            _promoteTries = 12;
        else
            _promoteTries++;

        var state = s.IndicatorState();
        _tray?.Update(state, s.Activity.Muted);
    }

    /// <summary>At most one queued UI callback; newer snapshots replace pending (latest wins).</summary>
    private void QueueLatestStatus(DispatcherQueue queue, HealthSnapshot snapshot)
    {
        lock (_statusDispatchLock)
        {
            _pendingStatus = snapshot;
            if (_statusDispatchQueued) return;
            _statusDispatchQueued = true;
        }
        if (!queue.TryEnqueue(DrainLatestStatus))
        {
            lock (_statusDispatchLock)
            {
                _statusDispatchQueued = false;
                _pendingStatus = null;
            }
        }
    }

    private void DrainLatestStatus()
    {
        while (true)
        {
            HealthSnapshot? s;
            lock (_statusDispatchLock)
            {
                s = _pendingStatus;
                _pendingStatus = null;
                if (s == null)
                {
                    _statusDispatchQueued = false;
                    return;
                }
            }
            bool showPanel = s.Activity.EngineRunning && s.Dictation.ShowPanel;
            ApplyStatus(s);
            _window?.ApplyPushed(s);
            if (!_testOverlay)
                _panel?.Update(
                    showPanel, s.Dictation.DictText,
                    s.Dictation.HasUsableTarget, s.Dictation.PromptGlow);
        }
    }

    /// <summary>Sole UI driver: blocks in WaitModelStatus, marshals on change.</summary>
    private void StartDictationPush()
    {
        var uiQueue = DispatcherQueue.GetForCurrentThread();
        _pushThread = new Thread(() =>
        {
            ulong since = 0; // 0 ⇒ immediate first sample
            bool delivered = false;
            while (!_pushStop)
            {
                string json;
                try { json = Native.ModelStatusWait(since, 1000); }
                catch { Thread.Sleep(500); continue; }
                if (_pushStop) break;
                if (string.IsNullOrWhiteSpace(json) || json == "{}")
                {
                    // Engine down: wait returns immediately — pace to avoid hot spin.
                    Thread.Sleep(400);
                    continue;
                }
                var s = HealthSnapshot.FromJson(json);
                // Idle timeout returns same seq (StatusGate::wait_changed) — skip re-marshal.
                // Win liveness = payload presence; macOS also yields on engineRunning flip — keep split.
                bool changed = !delivered || s.StatusSeq != since;
                since = s.StatusSeq;
                if (!changed) continue;
                delivered = true;
                QueueLatestStatus(uiQueue, s);
            }
        })
        { IsBackground = true, Name = "dictation-push" };
        _pushThread.Start();
    }

    private void ExitApp()
    {
        if (_exiting) return;
        _exiting = true;
        _pushStop = true; // wakes within 1s wait cap
        _activate?.Set(); // end reactivation thread
        _panel?.Dispose();
        _tray?.Dispose();
        if (_hostingEngine)
        {
            // EngineStop has no native timeout — cap join so Quit can't hang.
            var stop = System.Threading.Tasks.Task.Run(Native.EngineStop);
            stop.Wait(TimeSpan.FromSeconds(5));
        }
        _window?.Close();
        Exit();
    }
}
