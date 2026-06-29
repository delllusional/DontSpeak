using System;
using System.Drawing.Drawing2D;
using System.Runtime.InteropServices;

namespace DontSpeak;

// Shared Win32 interop for the GDI+/layered-window UI pieces (DictationPanel, TrayIcon):
// the window-class registration, DC, and DIB-section P/Invokes + their structs that were
// otherwise duplicated in each file. Component-specific imports still live with their
// component. Pull these in with `using static DontSpeak.Win32;`.

/// <summary>WndProc signature for the hand-rolled Win32 windows (the layered overlay + the
/// tray's owner window). Held in a field by each owner so the GC can't collect the thunk.</summary>
internal delegate IntPtr WndProcDelegate(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);

[StructLayout(LayoutKind.Sequential)]
internal struct POINT { public int x; public int y; }

[StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
internal struct WNDCLASS
{
    public uint style;
    public IntPtr lpfnWndProc;
    public int cbClsExtra;
    public int cbWndExtra;
    public IntPtr hInstance;
    public IntPtr hIcon;
    public IntPtr hCursor;
    public IntPtr hbrBackground;
    public string? lpszMenuName;
    public string? lpszClassName;
}

[StructLayout(LayoutKind.Sequential)]
internal struct BITMAPINFOHEADER
{
    public uint biSize;
    public int biWidth;
    public int biHeight;
    public ushort biPlanes;
    public ushort biBitCount;
    public uint biCompression;
    public uint biSizeImage;
    public int biXPelsPerMeter;
    public int biYPelsPerMeter;
    public uint biClrUsed;
    public uint biClrImportant;
}

[StructLayout(LayoutKind.Sequential)]
internal struct BITMAPINFO
{
    public BITMAPINFOHEADER bmiHeader;
    public uint bmiColors0;
}

[StructLayout(LayoutKind.Sequential)]
internal struct SIZE { public int cx; public int cy; }

/// <summary>The per-blit alpha-blend for <c>UpdateLayeredWindow</c>: per-pixel
/// (premultiplied) alpha via <c>AlphaFormat = AC_SRC_ALPHA</c>, optionally scaled by
/// the whole-layer <c>SourceConstantAlpha</c> (the fade).</summary>
[StructLayout(LayoutKind.Sequential)]
internal struct BLENDFUNCTION
{
    public byte BlendOp;
    public byte BlendFlags;
    public byte SourceConstantAlpha;
    public byte AlphaFormat;
}

internal static class Win32
{
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
    internal static extern IntPtr GetModuleHandleW(string? name);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    internal static extern ushort RegisterClassW(ref WNDCLASS wc);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    internal static extern bool UnregisterClassW(string className, IntPtr hInstance);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    internal static extern IntPtr CreateWindowExW(uint exStyle, string className, string windowName,
        uint style, int x, int y, int w, int h, IntPtr parent, IntPtr menu, IntPtr instance, IntPtr param);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    internal static extern IntPtr DefWindowProcW(IntPtr hwnd, uint msg, IntPtr wparam, IntPtr lparam);

    [DllImport("user32.dll")]
    internal static extern bool DestroyWindow(IntPtr hwnd);

    [DllImport("user32.dll")]
    internal static extern IntPtr GetDC(IntPtr hwnd);

    [DllImport("user32.dll")]
    internal static extern int ReleaseDC(IntPtr hwnd, IntPtr hdc);

    [DllImport("gdi32.dll")]
    internal static extern bool DeleteObject(IntPtr obj);

    [DllImport("gdi32.dll")]
    internal static extern IntPtr CreateDIBSection(IntPtr hdc, ref BITMAPINFO pbmi, uint usage, out IntPtr bits, IntPtr section, uint offset);

    [DllImport("gdi32.dll")]
    internal static extern IntPtr CreateCompatibleDC(IntPtr hdc);

    [DllImport("gdi32.dll")]
    internal static extern bool DeleteDC(IntPtr hdc);

    [DllImport("gdi32.dll")]
    internal static extern IntPtr SelectObject(IntPtr hdc, IntPtr obj);

    [DllImport("user32.dll")]
    internal static extern bool ShowWindow(IntPtr hwnd, int cmd);

    [DllImport("user32.dll")]
    internal static extern int GetSystemMetrics(int index);

    // Set this process's explicit AppUserModelID so the taskbar AND Task Manager group the app
    // under its real identity (paired with the Start-menu shortcut's matching AppUserModelID,
    // which maps this id -> the shortcut name "DontSpeak"). Without it Windows falls back to the
    // exe base name ("ds-winui") for the group label. Must be called at startup before any UI.
    [DllImport("shell32.dll", CharSet = CharSet.Unicode, PreserveSig = true)]
    internal static extern int SetCurrentProcessExplicitAppUserModelID(
        [MarshalAs(UnmanagedType.LPWStr)] string appID);

    // The layered-window blit for the dictation overlay.
    [DllImport("user32.dll")]
    internal static extern bool UpdateLayeredWindow(IntPtr hwnd, IntPtr hdcDst, ref POINT pptDst,
        ref SIZE psize, IntPtr hdcSrc, ref POINT pptSrc, uint crKey, ref BLENDFUNCTION pblend, uint dwFlags);

    /// <summary>A rounded-rectangle GDI+ path with corner radius <paramref name="r"/> —
    /// the Win11 card shape used by the dictation overlay (card + glow rings).</summary>
    internal static GraphicsPath RoundedRect(float x, float y, float w, float h, float r)
    {
        float d = r * 2f;
        var p = new GraphicsPath();
        p.AddArc(x, y, d, d, 180, 90);                 // top-left
        p.AddArc(x + w - d, y, d, d, 270, 90);         // top-right
        p.AddArc(x + w - d, y + h - d, d, d, 0, 90);   // bottom-right
        p.AddArc(x, y + h - d, d, d, 90, 90);          // bottom-left
        p.CloseFigure();
        return p;
    }
}
