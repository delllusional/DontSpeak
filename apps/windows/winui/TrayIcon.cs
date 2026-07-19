using System;
using System.Drawing;
using System.Runtime.InteropServices;
using System.Windows.Input;
using Microsoft.UI.Xaml.Controls;
using Microsoft.Win32;
using static DontSpeak.Win32;

namespace DontSpeak;

/// <summary>
/// Tray icon via H.NotifyIcon (<see cref="H.NotifyIcon.TaskbarIcon"/>) — WinUI 3 has no
/// first-party NotifyIcon. Modern <see cref="MenuFlyout"/> (SecondWindow); brand glyph HICONs
/// from <see cref="BrandGlyph"/>. Trade-off vs hand-rolled Win32: glyphs captured at launch
/// (restart re-renders theme/DPI). Events + <see cref="Update"/> surface for App.
/// </summary>
internal sealed class TrayIcon : IDisposable
{
    internal enum IconState { Idle = 0, Recording = 1, Speaking = 2 }

    public event Action? OpenStatus;
    public event Action? Exit;

    private readonly H.NotifyIcon.TaskbarIcon _icon;
    private readonly IntPtr[] _hicons = new IntPtr[3];
    private readonly Icon[] _stateIcons = new Icon[3];
    private readonly IntPtr[] _mutedHicons = new IntPtr[3];
    private readonly Icon[] _mutedStateIcons = new Icon[3];
    private ToggleMenuFlyoutItem? _autostartItem;
    private ToggleMenuFlyoutItem? _muteItem;
    private int _lastState = -1;
    private bool _muted;
    private bool _disposed;

    public TrayIcon()
    {
        BuildIcons();

        _icon = new H.NotifyIcon.TaskbarIcon
        {
            ToolTipText = Loc.T("common.app_name"), // state is icon color
            ContextMenuMode = H.NotifyIcon.ContextMenuMode.SecondWindow, // modern MenuFlyout
            NoLeftClickDelay = true,
            ContextFlyout = BuildMenu(),
        };
        _icon.LeftClickCommand = new RelayCommand(() => OpenStatus?.Invoke());
        _icon.UpdateIcon(_stateIcons[0]);
        _icon.ForceCreate(); // we own lifetime, not XAML
    }

    private MenuFlyout BuildMenu()
    {
        _muteItem = new ToggleMenuFlyoutItem { Text = Loc.T("tray.mute"), IsChecked = _muted };
        _muteItem.Click += (_, _) => SetMuted(!_muted);

        var settings = new MenuFlyoutItem { Text = Loc.T("tray.settings") };
        settings.Click += (_, _) => OpenStatus?.Invoke();

        _autostartItem = new ToggleMenuFlyoutItem
        {
            Text = Loc.T("tray.start_at_login"),
            IsChecked = AutostartEnabled(),
        };
        // Toggle flips IsChecked itself; re-read registry so check matches what persisted.
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
        // Mute/autostart may change via MCP — refresh checkmarks on open.
        flyout.Opening += (_, _) =>
        {
            if (_muteItem != null) _muteItem.IsChecked = _muted;
            if (_autostartItem != null) _autostartItem.IsChecked = AutostartEnabled();
        };
        return flyout;
    }

    private void SetMuted(bool muted)
    {
        // Cache only when engine accepted — else icon lies until next status push.
        if (Native.SetMuted(muted)) _muted = muted;
        if (_muteItem != null) _muteItem.IsChecked = _muted;
        ApplyIcon();
    }

    private void ApplyIcon()
    {
        int i = _lastState < 0 ? 0 : _lastState;
        _icon.UpdateIcon((_muted ? _mutedStateIcons : _stateIcons)[i]);
    }

    /// <summary>Icon for (state, muted); muted set carries the slash. Tooltip is static at construct.</summary>
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

    public void Balloon(string title, string body)
    {
        if (_disposed) return;
        _icon.ShowNotification(title, body);
    }

    /// <summary>Pin out of Win11 tray overflow: HKCU\…\NotifyIconSettings\*\IsPromoted=1 when
    /// ExecutablePath matches us. Library owns registration — we only set the flag. True when
    /// found (stop retrying).</summary>
    public static bool PromoteInTray()
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
        catch { return true; } // registry blocked → balloon covers it
    }

    // parent\filename suffix — robust to shell known-folder GUID path prefixes.
    private static string TrailTwo(string path)
    {
        var file = System.IO.Path.GetFileName(path);
        var dir = System.IO.Path.GetFileName(System.IO.Path.GetDirectoryName(path) ?? "");
        return dir.Length > 0 ? dir + "\\" + file : file;
    }

    private const string RunKey = @"Software\Microsoft\Windows\CurrentVersion\Run";
    private const string RunValue = "DontSpeak";

    public static bool AutostartEnabled()
    {
        try
        {
            using var k = Registry.CurrentUser.OpenSubKey(RunKey);
            return k?.GetValue(RunValue) != null;
        }
        catch { return false; }
    }

    /// <summary>Toggle HKCU Run. Enable → <c>--hidden</c> (resident tray host).</summary>
    public static void ToggleAutostart()
    {
        try
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
        catch { /* registry blocked — fail-soft like PromoteInTray */ }
    }

    private void BuildIcons()
    {
        int px = TrayIconPx();
        DestroyIcons();
        var inks = new[] { BrandGlyph.IdleForeground(), Brand.MicOrangeGdi, Brand.SeedPurpleGdi };
        for (int i = 0; i < 3; i++)
        {
            _hicons[i] = MakeGlyphIcon(px, inks[i], muted: false);
            _stateIcons[i] = Icon.FromHandle(_hicons[i]);
            _mutedHicons[i] = MakeGlyphIcon(px, inks[i], muted: true);
            _mutedStateIcons[i] = Icon.FromHandle(_mutedHicons[i]);
        }
    }

    private void DestroyIcons()
    {
        foreach (var ic in _stateIcons) ic?.Dispose();
        foreach (var ic in _mutedStateIcons) ic?.Dispose();
        foreach (var h in _hicons) if (h != IntPtr.Zero) DestroyIcon(h);
        foreach (var h in _mutedHicons) if (h != IntPtr.Zero) DestroyIcon(h);
    }

    /// <summary>Shell small-icon metric at system DPI (PerMonitorV2). Fallback 32 (~200%).</summary>
    private static int TrayIconPx()
    {
        try
        {
            uint dpi = GetDpiForSystem();
            if (dpi == 0) dpi = 96;
            int px = GetSystemMetricsForDpi(SM_CXSMICON, dpi);
            if (px > 0) return px;
        }
        catch { /* pre-1607 */ }
        return 32;
    }

    /// <summary>Premultiplied HICON via 32bpp DIB (DDB drops alpha → invisible icon).</summary>
    private static IntPtr MakeGlyphIcon(int size, Color ink, bool muted)
    {
        int W = size, H = size;
        var bmi = new BITMAPINFO
        {
            bmiHeader = new BITMAPINFOHEADER
            {
                biSize = (uint)Marshal.SizeOf<BITMAPINFOHEADER>(),
                biWidth = W,
                biHeight = -H, // top-down
                biPlanes = 1,
                biBitCount = 32,
                biCompression = 0,
            },
        };

        IntPtr hdc = GetDC(IntPtr.Zero);
        IntPtr color = CreateDIBSection(hdc, ref bmi, 0, out IntPtr bits, IntPtr.Zero, 0);
        _ = ReleaseDC(IntPtr.Zero, hdc);
        if (color == IntPtr.Zero) return LoadIconW(IntPtr.Zero, IDI_APPLICATION);

        // COPY into DIB — GDI+ draw onto external DIB bits is unreliable.
        var buf = BrandGlyph.RenderBgra(size, ink, muted);
        Marshal.Copy(buf, 0, bits, buf.Length);

        // 1bpp mask all 0 (alpha drives transparency). CreateBitmap needs WORD-aligned stride;
        // W/8 under-allocates at 125%/150% DPI and native overreads the heap.
        int maskStride = (W + 15) / 16 * 2;
        var mask = new byte[maskStride * H];
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
        _icon.Dispose();
        DestroyIcons();
    }

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

    /// <summary>Minimal ICommand for LeftClickCommand (avoids CommunityToolkit.Mvvm for one command).</summary>
    private sealed class RelayCommand : ICommand
    {
        private readonly Action _run;
        public RelayCommand(Action run) => _run = run;
        public event EventHandler? CanExecuteChanged { add { } remove { } }
        public bool CanExecute(object? parameter) => true;
        public void Execute(object? parameter) => _run();
    }
}
