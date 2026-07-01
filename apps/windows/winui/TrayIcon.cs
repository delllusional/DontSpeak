using System;
using System.Drawing; // System.Drawing.Color + Icon for the brand-glyph HICONs
using System.Runtime.InteropServices;
using System.Windows.Input; // ICommand for the tray's left-click command
using Microsoft.UI.Xaml.Controls; // MenuFlyout + items (the modern context menu)
using Microsoft.Win32;
using static DontSpeak.Win32; // shared DC / DIB-section interop (GetDC, CreateDIBSection, …)

namespace DontSpeak;

/// <summary>
/// The notification-area (system-tray) icon for the merged DontSpeak app. It now wraps
/// <c>H.NotifyIcon</c>'s <see cref="H.NotifyIcon.TaskbarIcon"/> — the de-facto WinUI tray
/// library (Microsoft ships no first-party NotifyIcon for WinUI 3) — so the right-click
/// menu is a modern translucent <see cref="MenuFlyout"/> (the <c>SecondWindow</c> mode)
/// instead of a classic Win32 popup, and the app can drop to efficiency mode in the tray.
///
/// We still render the brand glyph (assets/tray-icon.svg) to a per-state HICON via
/// <see cref="BrandGlyph"/> + GDI+ and hand it to the library as the icon, so the
/// state-tinted look (idle = theme fg, recording = orange/STT, speaking = purple/TTS) is
/// unchanged. The caller drives the icon via <see cref="Update"/> and wires the menu
/// actions through the public events — the SAME surface as before, so the app is untouched.
///
/// Trade-off vs the old hand-rolled Win32 icon: we no longer run our own message window,
/// so the icon is NOT auto-rebuilt on a live theme/DPI change (it reflects the state at
/// launch); a restart re-renders it. The modern menu + efficiency mode are the win.
/// </summary>
internal sealed class TrayIcon : IDisposable
{
    internal enum IconState { Idle = 0, Recording = 1, Speaking = 2 }

    // Menu commands — settings opens the app window; the rest mirror the macOS menu-bar app.
    public event Action? OpenStatus;
    public event Action? Exit;

    private readonly H.NotifyIcon.TaskbarIcon _icon;
    private readonly IntPtr[] _hicons = new IntPtr[3];        // owned HICONs (destroyed on Dispose)
    private readonly Icon[] _stateIcons = new Icon[3];        // System.Drawing.Icon wrappers over them
    private readonly IntPtr[] _mutedHicons = new IntPtr[3];   // the same three with a muted slash
    private readonly Icon[] _mutedStateIcons = new Icon[3];
    private ToggleMenuFlyoutItem? _autostartItem;
    private ToggleMenuFlyoutItem? _muteItem;
    private int _lastState = -1;
    private bool _muted;                                       // last-known global mute (drives the slashed icon)
    private bool _disposed;

    public TrayIcon()
    {
        BuildIcons(); // idle + the two active states (recording=STT, speaking=TTS)

        _icon = new H.NotifyIcon.TaskbarIcon
        {
            ToolTipText = Loc.T("common.app_name"), // static product-name label; icon color conveys state
            // The modern translucent flyout (a second transparent window renders it); the
            // default PopupMenu mode would draw a classic Win32 menu instead.
            ContextMenuMode = H.NotifyIcon.ContextMenuMode.SecondWindow,
            NoLeftClickDelay = true, // left-click opens the window immediately (no dbl-click wait)
            ContextFlyout = BuildMenu(),
        };
        // Left-click the tray icon → open the Status window (mirrors the old WM_LBUTTONUP).
        _icon.LeftClickCommand = new RelayCommand(() => OpenStatus?.Invoke());
        _icon.UpdateIcon(_stateIcons[0]);
        _icon.ForceCreate(); // create the Shell_NotifyIcon now (we manage lifetime, not XAML)
    }

    /// <summary>The right-click context menu as a modern <see cref="MenuFlyout"/>: Mute, Settings,
    /// Start at login (checked = enabled), Exit. Built in code so the strings come from the i18n
    /// catalog and the Mute / Start-at-login checkmarks track the live engine + Run-key state.</summary>
    private MenuFlyout BuildMenu()
    {
        // Mute: silences the voice without stopping playback; the tray icon shows a slash while
        // muted (mirrors macOS). Checkmark tracks the live engine mute state.
        _muteItem = new ToggleMenuFlyoutItem { Text = Loc.T("tray.mute"), IsChecked = _muted };
        _muteItem.Click += (_, _) => SetMuted(!_muted);

        var settings = new MenuFlyoutItem { Text = Loc.T("tray.settings") };
        settings.Click += (_, _) => OpenStatus?.Invoke();

        _autostartItem = new ToggleMenuFlyoutItem
        {
            Text = Loc.T("tray.start_at_login"),
            IsChecked = AutostartEnabled(),
        };
        // A ToggleMenuFlyoutItem flips IsChecked itself; re-read the registry so the check
        // reflects what actually persisted (and not a divergence).
        _autostartItem.Click += (_, _) =>
        {
            ToggleAutostart();
            _autostartItem.IsChecked = AutostartEnabled();
        };

        var exit = new MenuFlyoutItem { Text = Loc.T("tray.exit") };
        exit.Click += (_, _) => Exit?.Invoke();

        var flyout = new MenuFlyout();
        flyout.Items.Add(_muteItem);
        flyout.Items.Add(new MenuFlyoutSeparator());
        flyout.Items.Add(settings);
        flyout.Items.Add(new MenuFlyoutSeparator());
        flyout.Items.Add(_autostartItem);
        flyout.Items.Add(new MenuFlyoutSeparator());
        flyout.Items.Add(exit);
        // Refresh the checkmarks each time the menu opens (mute/autostart may have changed
        // elsewhere — e.g. via the MCP, or the macOS-parity engine state).
        flyout.Opening += (_, _) =>
        {
            if (_muteItem != null) _muteItem.IsChecked = _muted;
            if (_autostartItem != null) _autostartItem.IsChecked = AutostartEnabled();
        };
        return flyout;
    }

    /// <summary>Toggle global mute via the engine, update the cached state + the slashed icon
    /// immediately (don't wait for the next poll), and sync the menu checkmark.</summary>
    private void SetMuted(bool muted)
    {
        // Only cache the new state if the request reached the engine — otherwise the
        // icon would show muted while audio keeps playing (until the next poll).
        if (Native.SetMuted(muted)) _muted = muted;
        if (_muteItem != null) _muteItem.IsChecked = _muted;
        ApplyIcon();
    }

    /// <summary>Push the icon for the current (state, muted) — the muted set carries the slash.</summary>
    private void ApplyIcon()
    {
        int i = _lastState < 0 ? 0 : _lastState;
        _icon.UpdateIcon((_muted ? _mutedStateIcons : _stateIcons)[i]);
    }

    /// <summary>Refresh the icon for the current engine state + mute (the engine's `muted` flag
    /// is polled in, so the slash also reflects mute toggled elsewhere). The hover tooltip is a
    /// static product-name label set at construction, so it isn't touched here.</summary>
    public void Update(IconState state, bool muted)
    {
        if (_disposed) return;
        if (_lastState != (int)state || _muted != muted)
        {
            _lastState = (int)state;
            _muted = muted;
            ApplyIcon();
        }
    }

    /// <summary>Show a one-shot tray notification (the H.NotifyIcon balloon analogue).</summary>
    public void Balloon(string title, string body)
    {
        if (_disposed) return;
        _icon.ShowNotification(title, body);
    }

    /// <summary>Best-effort: pin this icon out of the Win11 tray OVERFLOW. Win11 stores
    /// per-icon visibility under HKCU\Control Panel\NotifyIconSettings\&lt;hash&gt; (one subkey
    /// per icon, carrying "ExecutablePath"); IsPromoted=1 pins it. We only SET the flag here
    /// (the library owns the icon registration, so we don't re-add it); the shell applies it
    /// on its next read. Returns true once our entry exists (found → stop retrying).</summary>
    public bool PromoteInTray()
    {
        var exe = Environment.ProcessPath;
        if (string.IsNullOrEmpty(exe)) return true;
        var tail = TrailTwo(exe);
        try
        {
            using var root = Registry.CurrentUser.OpenSubKey(
                @"Control Panel\NotifyIconSettings", writable: true);
            if (root == null) return false;
            bool found = false;
            foreach (var name in root.GetSubKeyNames())
            {
                using var k = root.OpenSubKey(name, writable: true);
                if (k?.GetValue("ExecutablePath") is not string p) continue;
                if (!(string.Equals(p, exe, StringComparison.OrdinalIgnoreCase) ||
                      p.EndsWith(tail, StringComparison.OrdinalIgnoreCase)))
                    continue;
                found = true;
                if (k.GetValue("IsPromoted") is not int v || v != 1)
                    k.SetValue("IsPromoted", 1, RegistryValueKind.DWord);
            }
            return found;
        }
        catch { return true; } // registry blocked → give up (the balloon hint covers it)
    }

    // "<parent-folder>\<filename>" of a path — the suffix we match tray entries on, robust to
    // the shell's known-folder GUID prefixing of the full path.
    private static string TrailTwo(string path)
    {
        var file = System.IO.Path.GetFileName(path);
        var dir = System.IO.Path.GetFileName(System.IO.Path.GetDirectoryName(path) ?? "");
        return dir.Length > 0 ? dir + "\\" + file : file;
    }

    // ── Start-at-login (HKCU\…\Run) ──────────────────────────────────────────
    private const string RunKey = @"Software\Microsoft\Windows\CurrentVersion\Run";
    private const string RunValue = "DontSpeak";

    public static bool AutostartEnabled()
    {
        using var k = Registry.CurrentUser.OpenSubKey(RunKey);
        return k?.GetValue(RunValue) != null;
    }

    /// <summary>Toggle the Run-key entry. When enabling, register the app to start minimized
    /// to the tray (<c>--hidden</c>), matching the resident-host model.</summary>
    public static void ToggleAutostart()
    {
        using var k = Registry.CurrentUser.CreateSubKey(RunKey);
        if (k == null) return;
        if (k.GetValue(RunValue) != null)
        {
            k.DeleteValue(RunValue, throwOnMissingValue: false);
        }
        else
        {
            var exe = Environment.ProcessPath ?? "";
            if (exe.Length > 0) k.SetValue(RunValue, $"\"{exe}\" --hidden");
        }
    }

    // ── brand-glyph icons (unchanged rendering; see BrandGlyph) ───────────────────────
    /// <summary>(Re)build all three state icons at the tray-icon size: idle = theme
    /// foreground, recording = mic-orange (STT), speaking = seed-purple (TTS).</summary>
    private void BuildIcons()
    {
        int px = TrayIconPx();
        DestroyIcons();
        var inks = new[] { BrandGlyph.IdleForeground(), Brand.MicOrangeGdi, Brand.SeedPurpleGdi };
        for (int i = 0; i < 3; i++)
        {
            _hicons[i] = MakeGlyphIcon(px, inks[i], muted: false);
            _stateIcons[i] = Icon.FromHandle(_hicons[i]);
            _mutedHicons[i] = MakeGlyphIcon(px, inks[i], muted: true);   // same glyph + a muted slash
            _mutedStateIcons[i] = Icon.FromHandle(_mutedHicons[i]);
        }
    }

    private void DestroyIcons()
    {
        foreach (var ic in _stateIcons) ic?.Dispose();        // wrappers (don't own the handle)
        foreach (var ic in _mutedStateIcons) ic?.Dispose();
        foreach (var h in _hicons) if (h != IntPtr.Zero) DestroyIcon(h);
        foreach (var h in _mutedHicons) if (h != IntPtr.Zero) DestroyIcon(h);
    }

    /// <summary>The pixel size the shell displays a notification-area icon at, at the system
    /// DPI: the small-icon metric (16px @ 96 DPI) scaled to the system DPI (our manifest is
    /// PerMonitorV2). Falls back to 32 (covers up to 200%).</summary>
    private static int TrayIconPx()
    {
        try
        {
            uint dpi = GetDpiForSystem();
            if (dpi == 0) dpi = 96;
            int px = GetSystemMetricsForDpi(SM_CXSMICON, dpi);
            if (px > 0) return px;
        }
        catch { /* pre-1607 build — fall through */ }
        return 32;
    }

    /// <summary>Build a <paramref name="size"/>×<paramref name="size"/> premultiplied-alpha
    /// tray icon: the one-color <paramref name="ink"/> "&lt;/&gt;" bubble (assets/tray-icon.svg),
    /// via a 32bpp DIB section (a DDB would drop the alpha → an invisible icon).</summary>
    private static IntPtr MakeGlyphIcon(int size, Color ink, bool muted)
    {
        int W = size, H = size;
        var bmi = new BITMAPINFO
        {
            bmiHeader = new BITMAPINFOHEADER
            {
                biSize = (uint)Marshal.SizeOf<BITMAPINFOHEADER>(),
                biWidth = W,
                biHeight = -H, // top-down rows
                biPlanes = 1,
                biBitCount = 32,
                biCompression = 0, // BI_RGB
            },
        };

        IntPtr hdc = GetDC(IntPtr.Zero);
        IntPtr color = CreateDIBSection(hdc, ref bmi, 0, out IntPtr bits, IntPtr.Zero, 0);
        ReleaseDC(IntPtr.Zero, hdc);
        if (color == IntPtr.Zero) return LoadIconW(IntPtr.Zero, IDI_APPLICATION);

        // Render the mark to a straight-alpha BGRA buffer (shared with the window glyph) and
        // COPY it into the DIB (drawing GDI+ onto external DIB memory is unreliable).
        var buf = BrandGlyph.RenderBgra(size, ink, muted);
        Marshal.Copy(buf, 0, bits, buf.Length);

        var mask = new byte[(W / 8) * H]; // 1bpp mask, all 0 — the 32bpp alpha drives transparency
        IntPtr hbmMask = CreateBitmap(W, H, 1, 1, mask);
        var ii = new ICONINFO { fIcon = true, hbmMask = hbmMask, hbmColor = color };
        IntPtr icon = CreateIconIndirect(ref ii);
        DeleteObject(color);
        DeleteObject(hbmMask);
        return icon != IntPtr.Zero ? icon : LoadIconW(IntPtr.Zero, IDI_APPLICATION);
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        _icon.Dispose();   // removes the Shell_NotifyIcon + tears down the flyout window
        DestroyIcons();
    }

    // ── constants + interop NOT covered by Win32.cs ───────────────────────────────────
    private const int SM_CXSMICON = 49;
    private static readonly IntPtr IDI_APPLICATION = (IntPtr)32512;

    [StructLayout(LayoutKind.Sequential)]
    private struct ICONINFO
    {
        [MarshalAs(UnmanagedType.Bool)] public bool fIcon;
        public int xHotspot;
        public int yHotspot;
        public IntPtr hbmMask;
        public IntPtr hbmColor;
    }

    [DllImport("user32.dll")]
    private static extern uint GetDpiForSystem();

    [DllImport("user32.dll")]
    private static extern int GetSystemMetricsForDpi(int index, uint dpi);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr LoadIconW(IntPtr hinst, IntPtr name);

    [DllImport("user32.dll")]
    private static extern bool DestroyIcon(IntPtr icon);

    [DllImport("user32.dll")]
    private static extern IntPtr CreateIconIndirect(ref ICONINFO ii);

    [DllImport("gdi32.dll")]
    private static extern IntPtr CreateBitmap(int w, int h, uint planes, uint bitCount, byte[] bits);

    /// <summary>Minimal always-executable ICommand for the tray's LeftClickCommand (avoids a
    /// CommunityToolkit.Mvvm dependency for one command).</summary>
    private sealed class RelayCommand : ICommand
    {
        private readonly Action _run;
        public RelayCommand(Action run) => _run = run;
        public event EventHandler? CanExecuteChanged { add { } remove { } }
        public bool CanExecute(object? parameter) => true;
        public void Execute(object? parameter) => _run();
    }
}
