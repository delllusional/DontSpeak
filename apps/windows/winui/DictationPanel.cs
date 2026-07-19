using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Numerics;
using System.Runtime.InteropServices;
using System.Threading;
using Microsoft.Graphics.Canvas;
using Microsoft.Graphics.Canvas.Effects;
using Microsoft.Graphics.Canvas.Geometry;
using Microsoft.Graphics.Canvas.Text;
using Windows.Foundation;
using Windows.Graphics.DirectX;
using Vortice.DirectComposition;
using Vortice.DXGI;
using static DontSpeak.Win32; // shared window-class interop + structs
using WinColor = Windows.UI.Color;

namespace DontSpeak;

/// <summary>
/// Dictation overlay (macOS OverlayPanel): non-activating topmost Win32 window + Win2D swap
/// chain hosted via DirectComposition (no UpdateLayeredWindow / GPU→CPU readback). Fixed max
/// height container — card top-anchored, grows down into HTTRANSPARENT slack (no 1-frame wrap
/// flicker). Not WinUI: must never activate; App SDK compositor lacks ICompositorDesktopInterop
/// for bare HWND. Dedicated render thread (vsync Present(1); not thread pool — STT starves it);
/// Update publishes Snapshot only. Self-heals GPU/DComp on device loss. SWP_ASYNCWINDOWPOS so
/// render never blocks UI Dispose join. Driven by App status push via <see cref="Update"/>.
/// </summary>
internal sealed class DictationPanel : IDisposable
{
    private readonly WndProcDelegate _wndProc; // keep alive — GC of thunk crashes WndProc
    private readonly IntPtr _hwnd;
    private bool _disposed;
    private readonly Stopwatch _clock = Stopwatch.StartNew();

    // UI writes Snapshot; render reads once/frame (lock-free; UI serializes writers).
    private volatile Snapshot _snap = Snapshot.Empty;

    // Drag/resize: WndProc writes, PlaceWindow reads — out of band with Update.
    private volatile bool _userMoved;
    private volatile int _userPosX, _userPosY;
    private volatile bool _dragging;
    private volatile int _userWidth;      // 0 = CardWidth
    // Card bottom in window px for hit-test click-through below card. 0 until first frame.
    private volatile int _cardBottomInWin;

    private readonly Thread _renderThread;
    private readonly ManualResetEventSlim _wake = new(false);
    private volatile bool _stop;
    private int _renderFailures;
    private const int MaxRenderFailures = 8; // then idle until next signal (no hot-loop)

    /// <summary>Immutable frame input. `record` for safe `with` copies (OnThemeChanged).</summary>
    internal sealed record Snapshot
    {
        public bool Visible;
        public bool GlowOn;                 // listening OR no paste target
        public bool WholePill;              // no-target wash vs speak-now frame glow
        public bool Light;
        public WinColor Glow;
        public string[] Words = Array.Empty<string>();
        public long[] AppearMs = Array.Empty<long>();
        public string?[] OutWords = Array.Empty<string?>();
        public long[] OutAppearMs = Array.Empty<long>();
        public static readonly Snapshot Empty = new();
    }

    // GPU / Win2D — render-thread only.
    private CanvasDevice? _device;
    private CanvasTextFormat? _fmt;
    private CanvasSwapChain? _swapChain;
    private int _swapW, _swapH;
    private IDXGISwapChain1? _dxgiSwap;
    private float _lineH;
    private int _maxSurfaceH;                // fixed height; set once per device
    private readonly Dictionary<string, float> _wordW = new();
    private readonly Dictionary<string, CanvasRenderTarget> _tiles = new();
    private WinColor _tileColor;
    // Glow shape constant while breathing — bake Gaussian once per size; composite by opacity.
    private CanvasRenderTarget? _outerGlowTile;
    private CanvasRenderTarget? _frameGlowTile;
    private int _glowW, _glowH;

    private IDCompositionDevice? _dcompDevice;
    private IDCompositionTarget? _dcompTarget;
    private IDCompositionVisual? _dcompVisual;

    private bool _shownNative;
    private bool _wasDragging;
    private int _curX, _curY, _curW, _curH;

    // 96 DPI surface: 1 unit == 1 px (DPI scale is a later refinement).
    private const int CardWidth = 460;                          // macOS 460-pt pill
    private const int MinCardWidth = 240, MaxCardWidth = 900;
    private const int PadX = 18, PadY = 13, Radius = 14;
    private const int BottomMargin = 90;
    private const int GlowMargin = 26;
    private const int MaxExtraLines = 13;    // fixed-height budget (~14 lines; avoids resize flicker)
    // TUNE vs macOS for 60Hz (macOS parity: Fade 220, MaxBlur 6, TilePad 14).
    private const float FadeMs = 360f;
    private const int BreathMs = 2400;
    private const float FontSizeDip = 20f;   // 15pt @96
    private const float WordGap = 6f;        // DWrite drops trailing space
    private const float MaxBlur = 9f;
    private const float TilePad = 24f;       // keep ≥ ~2.6·MaxBlur
    private static readonly WinColor Transparent = WinColor.FromArgb(0, 0, 0, 0);

    public DictationPanel()
    {
        _wndProc = WndProc;
        IntPtr hinstance = GetModuleHandleW(null);
        var wc = new WNDCLASS
        {
            lpfnWndProc = Marshal.GetFunctionPointerForDelegate(_wndProc),
            hInstance = hinstance,
            lpszClassName = WndClassName,
        };
        RegisterClassW(ref wc);
        // No WS_EX_LAYERED (DComp can't share GDI layered path). NOREDIRECTIONBITMAP = clean
        // per-pixel alpha. No WS_EX_TRANSPARENT — card is draggable; NOACTIVATE keeps focus.
        _hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            WndClassName, "DontSpeak Dictation", WS_POPUP,
            0, 0, 0, 0, IntPtr.Zero, IntPtr.Zero, hinstance, IntPtr.Zero);

        _renderThread = new Thread(RenderLoop) { IsBackground = true, Name = "dictation-render" };
        _renderThread.Start();
    }

    /// <summary>UI thread: publish Snapshot + wake. <paramref name="promptGlow"/> is derived
    /// from canonical dictation state.</summary>
    public void Update(bool visible, string text, bool hasTarget, bool promptGlow)
    {
        if (_disposed || _hwnd == IntPtr.Zero) return;
        if (!visible)
        {
            if (_snap.Visible) { _snap = new Snapshot(); _wake.Set(); }
            return;
        }

        var prev = _snap;
        bool hasText = !string.IsNullOrWhiteSpace(text);
        // Same orange; shape differs: speak-now = frame, no-target = whole-pill wash.
        var s = new Snapshot
        {
            Visible = true,
            GlowOn = promptGlow || !hasTarget,
            WholePill = !hasTarget,
            Light = IsLightTheme(),
            Glow = Brand.Warning,
        };
        BuildWords(prev, s, hasText ? text.Trim() : "", _clock.ElapsedMilliseconds);
        _snap = s;
        _wake.Set();
    }

    /// <summary>Per-word fade diff (macOS blurReplace). Pure data; `now` injected so tests need
    /// no Win32 window. Unchanged prefix keeps stamps; replaced slot → OutWords.</summary>
    internal static void BuildWords(Snapshot prev, Snapshot s, string text, long now)
    {
        var nw = text.Length == 0
            ? Array.Empty<string>()
            : text.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries);
        var na = new long[nw.Length];
        var ow = new string?[nw.Length];
        var oa = new long[nw.Length];
        for (int i = 0; i < nw.Length; i++)
        {
            bool unchanged = i < prev.Words.Length && prev.Words[i] == nw[i];
            na[i] = unchanged ? prev.AppearMs[i] : now;
            if (unchanged)
            {
                // Carry in-flight outgoing fade across rapid re-renders.
                if (i < prev.OutWords.Length && prev.OutWords[i] != null && now - prev.OutAppearMs[i] < FadeMs)
                {
                    ow[i] = prev.OutWords[i];
                    oa[i] = prev.OutAppearMs[i];
                }
            }
            else if (i < prev.Words.Length && !string.IsNullOrEmpty(prev.Words[i]))
            {
                ow[i] = prev.Words[i];
                oa[i] = now;
            }
        }
        s.Words = nw;
        s.AppearMs = na;
        s.OutWords = ow;
        s.OutAppearMs = oa;
    }

    private void OnThemeChanged()
    {
        var prev = _snap;
        if (!prev.Visible) return;
        bool light = IsLightTheme();
        if (light == prev.Light) return;
        _snap = prev with { Light = light };
        _wake.Set();
    }

    /// <summary>Vsync-paced loop; self-heals GPU/DComp on any render failure. Reset-then-recheck
    /// _snap closes lost-wake race with Update.</summary>
    private void RenderLoop()
    {
        try
        {
            while (!_stop)
            {
                var s = _snap;
                bool animating = false;
                try
                {
                    if (!s.Visible)
                    {
                        if (_shownNative) { HideWindow(); _shownNative = false; }
                        ClearTiles();
                        _wordW.Clear();
                    }
                    else
                    {
                        animating = RenderOnce(s);
                    }
                    _renderFailures = 0;
                }
                catch (Exception ex) when (_device != null && _device.IsDeviceLost(ex.HResult))
                {
                    CleanupGpu();
                    _shownNative = false;
                    Thread.Sleep(200);
                    continue;
                }
                catch
                {
                    // Don't kill the thread — rebuild stack with backoff; after streak, idle.
                    CleanupGpu();
                    _shownNative = false;
                    if (++_renderFailures >= MaxRenderFailures)
                    {
                        _renderFailures = 0;
                        _wake.Reset();
                        if (ReferenceEquals(_snap, s) && !_stop) _wake.Wait();
                        continue;
                    }
                    Thread.Sleep(Math.Min(50 * _renderFailures, 500));
                    continue;
                }

                if (animating && !_stop) continue;

                _wake.Reset();
                if (ReferenceEquals(_snap, s) && !_stop) _wake.Wait();
            }
        }
        catch { /* teardown race */ }
        finally { CleanupGpu(); }
    }

    /// <summary>One frame into fixed-height swap chain. Returns true if still animating.</summary>
    private bool RenderOnce(Snapshot s)
    {
        EnsureDevice();

        long now = _clock.ElapsedMilliseconds;
        int cw = _userWidth > 0 ? Math.Clamp(_userWidth, MinCardWidth, MaxCardWidth) : CardWidth;
        int textArea = cw - PadX * 2;

        int th = (int)Math.Ceiling(LayoutWords(s, null, 0, 0, textArea, false, default, now));
        int cardW = cw, cardH = th + PadY * 2;
        int w = cardW + GlowMargin * 2;
        int h = _maxSurfaceH;
        int ox = GlowMargin, oy = GlowMargin;
        // Top-anchor: first line keeps screen Y as lines grow.
        int oneLineWinH = (int)Math.Ceiling(_lineH) + PadY * 2 + GlowMargin * 2;

        EnsureSwapChain(w, h);
        var cardRect = new Rect(ox, oy, cardW, cardH);

        // Time-based breath so intensity is refresh-rate independent.
        double phase = (now % BreathMs) / (double)BreathMs;
        bool fading = AnyFading(s, now);
        bool animating = s.GlowOn || fading;
        double gi = s.GlowOn ? 0.45 + 0.55 * (Math.Sin(phase * 2 * Math.PI) * 0.5 + 0.5) : 0;

        if (s.GlowOn) EnsureGlowResources(cardW, cardH, cardRect, w, h, s.Glow);

        using (var ds = _swapChain!.CreateDrawingSession(Transparent))
        {
            if (s.GlowOn) DrawOuterGlow(ds, gi);
            DrawCard(ds, cardRect, s.Light);
            if (s.GlowOn && s.WholePill)
            {
                int a = (int)(255 * (0.16 + 0.26 * gi));
                ds.FillRoundedRectangle(cardRect, Radius, Radius, Argb(a, s.Glow));
            }
            else if (s.GlowOn)
            {
                DrawFrameGlow(ds, cardRect, gi, s.Glow);
            }
            LayoutWords(s, ds, ox + PadX, oy + PadY, textArea, true, TextColor(s.Light), now);
        }
        _swapChain.Present(1);

        _cardBottomInWin = oy + cardH;

        PlaceWindow(w, h, oneLineWinH);
        return animating;
    }

    private static bool AnyFading(Snapshot s, long now)
    {
        for (int i = 0; i < s.AppearMs.Length; i++)
            if (now - s.AppearMs[i] < FadeMs) return true;
        for (int i = 0; i < s.OutWords.Length; i++)
            if (s.OutWords[i] != null && now - s.OutAppearMs[i] < FadeMs) return true;
        return false;
    }

    // ── GPU device / swap-chain / DComp lifecycle ────────────────────────────────────────────
    private void EnsureDevice()
    {
        if (_device != null) return;
        _device = CanvasDevice.GetSharedDevice();
        _fmt = new CanvasTextFormat
        {
            FontFamily = "Segoe UI",
            FontSize = FontSizeDip,
            HorizontalAlignment = CanvasHorizontalAlignment.Left,
            VerticalAlignment = CanvasVerticalAlignment.Top,
            WordWrapping = CanvasWordWrapping.NoWrap,
        };
        using var probe = new CanvasTextLayout(_device, "Ayg", _fmt, 1e6f, 1e6f);
        _lineH = (float)probe.LayoutBounds.Height;
        // Fixed height for device lifetime — avoids 1-frame wrap flicker.
        _maxSurfaceH = (int)Math.Ceiling(_lineH) + PadY * 2 + GlowMargin * 2
                       + MaxExtraLines * (int)Math.Ceiling(_lineH);
        _wordW.Clear();
        ClearTiles();
        _tileColor = default;
    }

    /// <summary>Create/resize composition swap chain; first create binds DComp. ResizeBuffers keeps
    /// the same object (visual stays bound). Height fixed → resize only on width change.</summary>
    private void EnsureSwapChain(int w, int h)
    {
        if (_swapChain == null)
        {
            _swapChain = new CanvasSwapChain(_device, w, h, 96f,
                DirectXPixelFormat.B8G8R8A8UIntNormalized, 2, CanvasAlphaMode.Premultiplied);
            _swapW = w; _swapH = h;
            SetupDComp();
            return;
        }
        if (w != _swapW || h != _swapH)
        {
            _swapChain.ResizeBuffers(w, h, 96f);
            _swapW = w; _swapH = h;
        }
    }

    private void SetupDComp()
    {
        // Vortice owns native IDXGISwapChain1 ref; same object is DComp content + device source.
        IntPtr nativeSwap = GetNativeSwapChain(_swapChain!);
        _dxgiSwap = new IDXGISwapChain1(nativeSwap);

        using IDXGIDevice dxgiDevice = _dxgiSwap.GetDevice<IDXGIDevice>();
        _dcompDevice = DComp.DCompositionCreateDevice<IDCompositionDevice>(dxgiDevice);

        _dcompDevice.CreateTargetForHwnd(_hwnd, true, out _dcompTarget).CheckError();
        _dcompVisual = _dcompDevice.CreateVisual();
        _dcompVisual.SetContent(_dxgiSwap);
        _dcompTarget.SetRoot(_dcompVisual);
        _dcompDevice.Commit().CheckError();
    }

    private float WordWidth(string word)
    {
        if (_wordW.TryGetValue(word, out float w)) return w;
        using var layout = new CanvasTextLayout(_device, word, _fmt, 1e6f, 1e6f);
        w = (float)layout.LayoutBounds.Width;
        _wordW[word] = w;
        return w;
    }

    private CanvasRenderTarget WordTile(string word, WinColor color)
    {
        if (!color.Equals(_tileColor)) { ClearTiles(); _tileColor = color; }
        if (_tiles.TryGetValue(word, out var tile)) return tile;
        float tw = WordWidth(word) + TilePad * 2;
        float ht = _lineH + TilePad * 2;
        tile = new CanvasRenderTarget(_device, tw, ht, 96f);
        using (var ds = tile.CreateDrawingSession())
        {
            ds.Clear(Transparent);
            ds.DrawText(word, new Vector2(TilePad, TilePad), color, _fmt);
        }
        _tiles[word] = tile;
        return tile;
    }

    private void ClearTiles()
    {
        foreach (var t in _tiles.Values) t.Dispose();
        _tiles.Clear();
    }

    /// <summary>Bake Gaussians once per card size; breath is opacity-only (~⅔ core → sliver).</summary>
    private void EnsureGlowResources(int cardW, int cardH, Rect card, int surfW, int surfH, WinColor glow)
    {
        if (_outerGlowTile != null && _glowW == cardW && _glowH == cardH) return;
        _outerGlowTile?.Dispose();
        _frameGlowTile?.Dispose();

        _outerGlowTile = new CanvasRenderTarget(_device, surfW, surfH, 96f);
        using (var cl = new CanvasCommandList(_device))
        {
            using (var cds = cl.CreateDrawingSession())
                cds.FillRoundedRectangle(card, Radius, Radius, glow);
            using var blur = new GaussianBlurEffect { Source = cl, BlurAmount = 14f, BorderMode = EffectBorderMode.Soft };
            using var ds = _outerGlowTile.CreateDrawingSession();
            ds.Clear(Transparent);
            ds.DrawImage(blur);
        }

        _frameGlowTile = new CanvasRenderTarget(_device, surfW, surfH, 96f);
        using (var cl = new CanvasCommandList(_device))
        {
            using (var cds = cl.CreateDrawingSession())
                cds.DrawRoundedRectangle(card, Radius, Radius, glow, 2.4f);
            using var blur = new GaussianBlurEffect { Source = cl, BlurAmount = 6f, BorderMode = EffectBorderMode.Soft };
            using var clip = CanvasGeometry.CreateRoundedRectangle(_device, card, Radius, Radius);
            using var ds = _frameGlowTile.CreateDrawingSession();
            ds.Clear(Transparent);
            using (ds.CreateLayer(1f, clip))
                ds.DrawImage(blur);
        }

        _glowW = cardW; _glowH = cardH;
    }

    private float LayoutWords(Snapshot s, CanvasDrawingSession? ds, float left, float top, float maxW, bool draw, WinColor baseColor, long now)
    {
        float x = left, y = top;
        for (int i = 0; i < s.Words.Length; i++)
        {
            float ww = WordWidth(s.Words[i]);
            if (x > left && x + ww > left + maxW) { x = left; y += _lineH; }
            if (draw && ds != null) DrawWordAt(s, ds, i, x, y, baseColor, now);
            x += ww + WordGap;
        }
        return (y - top) + _lineH;
    }

    private void DrawWordAt(Snapshot s, CanvasDrawingSession ds, int i, float x, float y, WinColor baseColor, long now)
    {
        if (i < s.OutWords.Length && s.OutWords[i] is string outW)
        {
            float q = Math.Clamp((now - s.OutAppearMs[i]) / FadeMs, 0f, 1f);
            if (q < 1f)
            {
                float qe = Ease(q);
                DrawTile(ds, outW, x, y, baseColor, qe * MaxBlur, 1f - qe);
            }
        }
        float p = Math.Clamp((now - s.AppearMs[i]) / FadeMs, 0f, 1f);
        if (p >= 1f)
        {
            ds.DrawText(s.Words[i], new Vector2(x, y), baseColor, _fmt);
        }
        else
        {
            float pe = Ease(p);
            DrawTile(ds, s.Words[i], x, y, baseColor, (1f - pe) * MaxBlur, pe);
        }
    }

    /// <summary>Fresh effect pair per call: D2D realizes effect graphs lazily — reusing one mutable
    /// effect for out+in DrawImage makes both use last params (replace becomes plain swap).</summary>
    private void DrawTile(CanvasDrawingSession ds, string word, float x, float y, WinColor color, float blur, float opacity)
    {
        var tile = WordTile(word, color);
        ICanvasImage img = tile;
        GaussianBlurEffect? blurFx = null;
        OpacityEffect? opFx = null;
        if (blur > 0.05f)
        {
            blurFx = new GaussianBlurEffect { Source = img, BlurAmount = blur, BorderMode = EffectBorderMode.Soft };
            img = blurFx;
        }
        if (opacity < 0.999f)
        {
            opFx = new OpacityEffect { Source = img, Opacity = Math.Clamp(opacity, 0f, 1f) };
            img = opFx;
        }
        ds.DrawImage(img, new Vector2(x - TilePad, y - TilePad));
        opFx?.Dispose();
        blurFx?.Dispose();
    }

    private static float Ease(float t) => t < 0.5f ? 2f * t * t : 1f - 2f * (1f - t) * (1f - t);

    private void DrawOuterGlow(CanvasDrawingSession ds, double intensity)
    {
        var r = new Rect(0, 0, _outerGlowTile!.Size.Width, _outerGlowTile.Size.Height);
        ds.DrawImage(_outerGlowTile, r, r, (float)Math.Clamp(intensity * 0.45, 0, 1));
    }

    private void DrawFrameGlow(CanvasDrawingSession ds, Rect card, double intensity, WinColor glow)
    {
        var r = new Rect(0, 0, _frameGlowTile!.Size.Width, _frameGlowTile.Size.Height);
        ds.DrawImage(_frameGlowTile, r, r, (float)Math.Clamp(intensity * 0.9, 0, 1));
        ds.DrawRoundedRectangle(card, Radius, Radius, Argb((int)Math.Clamp(intensity * 140, 0, 255), glow), 1.4f);
    }

    private static void DrawCard(CanvasDrawingSession ds, Rect rect, bool light)
    {
        // Flat glass tint (≥~0.8 acrylic baseline) — no live backdrop blur on a composed HWND.
        WinColor bg = light ? WinColor.FromArgb(210, 244, 244, 247) : WinColor.FromArgb(204, 28, 28, 32);
        ds.FillRoundedRectangle(rect, Radius, Radius, bg);
        WinColor border = light ? WinColor.FromArgb(30, 0, 0, 0) : WinColor.FromArgb(34, 255, 255, 255);
        ds.DrawRoundedRectangle(rect, Radius, Radius, border, 1f);
    }

    private static WinColor TextColor(bool light) =>
        light ? WinColor.FromArgb(255, 24, 24, 28) : WinColor.FromArgb(255, 240, 240, 245);

    private static WinColor Argb(int a, WinColor c) => WinColor.FromArgb((byte)Math.Clamp(a, 0, 255), c.R, c.G, c.B);

    private static bool IsLightTheme()
    {
        try
        {
            using var k = Microsoft.Win32.Registry.CurrentUser.OpenSubKey(
                @"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
            return (k?.GetValue("AppsUseLightTheme") as int?) == 1;
        }
        catch { return false; }
    }

    /// <summary>SWP_ASYNCWINDOWPOS — cross-thread, no block on UI (needed at Dispose join).
    /// SetWindowPos only on show/move/width change.</summary>
    private void PlaceWindow(int w, int h, int oneLineWinH)
    {
        if (_dragging)
        {
            _wasDragging = true;
            return;
        }
        if (_wasDragging)
        {
            _wasDragging = false;
            if (GetWindowRect(_hwnd, out RECT r))
            {
                _curX = r.left; _curY = r.top; _curW = r.right - r.left; _curH = r.bottom - r.top;
            }
        }

        int x, y;
        if (_userMoved)
        {
            x = _userPosX;
            y = _userPosY;
        }
        else
        {
            GetWorkArea(out RECT wa);
            x = wa.left + ((wa.right - wa.left) - w) / 2;
            y = wa.bottom - oneLineWinH - BottomMargin;
        }

        if (!_shownNative)
        {
            SetWindowPos(_hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_ASYNCWINDOWPOS);
            _shownNative = true;
            _curX = x; _curY = y; _curW = w; _curH = h;
        }
        else if (x != _curX || y != _curY || w != _curW || h != _curH)
        {
            SetWindowPos(_hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS);
            _curX = x; _curY = y; _curW = w; _curH = h;
        }
    }

    private void HideWindow() =>
        SetWindowPos(_hwnd, IntPtr.Zero, 0, 0, 0, 0,
            SWP_HIDEWINDOW | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS);

    private static void GetWorkArea(out RECT rect)
    {
        rect = default;
        if (!SystemParametersInfoW(SPI_GETWORKAREA, 0, ref rect, 0))
        {
            rect = new RECT { left = 0, top = 0, right = GetSystemMetrics(SM_CXSCREEN), bottom = GetSystemMetrics(SM_CYSCREEN) };
        }
    }

    // Card = caption (drag); slack below = click-through; pin drop via WM_EXITSIZEMOVE.
    private IntPtr WndProc(IntPtr hwnd, uint msg, IntPtr wparam, IntPtr lparam)
    {
        switch (msg)
        {
            case WM_MOUSEACTIVATE:
                return (IntPtr)MA_NOACTIVATE;
            case WM_SETTINGCHANGE:
                // Theme flip doesn't bump status seq — must listen here or card stays stale.
                if (lparam != IntPtr.Zero && Marshal.PtrToStringUni(lparam) == "ImmersiveColorSet")
                    OnThemeChanged();
                break;
            case WM_NCHITTEST:
            {
                // ToInt32() overflows when bit 31 set (monitors left/above primary) — kills process.
                int lp = unchecked((int)lparam.ToInt64());
                int sx = (short)(lp & 0xFFFF);
                int sy = (short)((lp >> 16) & 0xFFFF);
                if (GetWindowRect(hwnd, out RECT wr))
                {
                    int relx = sx - wr.left, rely = sy - wr.top, width = wr.right - wr.left;
                    int cardBottom = _cardBottomInWin;
                    if (rely < GlowMargin || (cardBottom > 0 && rely > cardBottom))
                        return (IntPtr)HTTRANSPARENT;
                    const int grip = GlowMargin + 10;
                    if (relx <= grip) return (IntPtr)HTLEFT;
                    if (relx >= width - grip) return (IntPtr)HTRIGHT;
                }
                return (IntPtr)HTCAPTION;
            }
            case WM_SETCURSOR:
            {
                int ht = (int)(lparam.ToInt64() & 0xFFFF);
                if (ht == HTLEFT || ht == HTRIGHT) { SetCursor(LoadCursorW(IntPtr.Zero, IDC_SIZEWE)); return (IntPtr)1; }
                if (ht == HTCAPTION) { SetCursor(LoadCursorW(IntPtr.Zero, IDC_SIZEALL)); return (IntPtr)1; }
                break;
            }
            case WM_SIZING:
            {
                // Horizontal only; height stays fixed container.
                var r = Marshal.PtrToStructure<RECT>(lparam);
                int clamped = Math.Clamp((r.right - r.left) - GlowMargin * 2, MinCardWidth, MaxCardWidth);
                _userWidth = clamped;
                int winW = clamped + GlowMargin * 2;
                if (unchecked((int)wparam.ToInt64()) is WMSZ_LEFT or WMSZ_TOPLEFT or WMSZ_BOTTOMLEFT)
                    r.left = r.right - winW;
                else
                    r.right = r.left + winW;
                Marshal.StructureToPtr(r, lparam, false);
                _wake.Set();
                return (IntPtr)1;
            }
            case WM_ENTERSIZEMOVE:
                _dragging = true;
                break;
            case WM_EXITSIZEMOVE:
                _dragging = false;
                if (GetWindowRect(hwnd, out RECT er))
                {
                    _userPosX = er.left;
                    _userPosY = er.top;
                    _userMoved = true;
                }
                _wake.Set();
                break;
        }
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }

    /// <summary>Render-thread only. Shared CanvasDevice dropped, not disposed.
    /// Order visual→target→device→swap balances SetContent refs.</summary>
    private void CleanupGpu()
    {
        try { _dcompVisual?.Dispose(); } catch { } _dcompVisual = null;
        try { _dcompTarget?.Dispose(); } catch { } _dcompTarget = null;
        try { _dcompDevice?.Dispose(); } catch { } _dcompDevice = null;
        try { _dxgiSwap?.Dispose(); } catch { } _dxgiSwap = null;
        _swapChain?.Dispose(); _swapChain = null; _swapW = _swapH = 0;
        ClearTiles();
        _outerGlowTile?.Dispose(); _outerGlowTile = null;
        _frameGlowTile?.Dispose(); _frameGlowTile = null;
        _glowW = _glowH = 0;
        _fmt?.Dispose(); _fmt = null;
        _device = null;
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        _stop = true;
        _wake.Set();
        // Bounded join; SWP_ASYNCWINDOWPOS on render side avoids deadlock with UI join.
        try { _renderThread.Join(1500); } catch { }
        _wake.Dispose();
        if (_hwnd != IntPtr.Zero) DestroyWindow(_hwnd);
        UnregisterClassW(WndClassName, GetModuleHandleW(null));
    }

    /// <summary>CsWinRT Win2D → native IDXGISwapChain1* via ICanvasResourceWrapperNative. Owned ref.</summary>
    private static IntPtr GetNativeSwapChain(CanvasSwapChain swapChain)
    {
        IntPtr insp = WinRT.MarshalInspectable<object>.FromManaged(swapChain);
        try
        {
            object rcw = Marshal.GetObjectForIUnknown(insp);
            var wrap = (ICanvasResourceWrapperNative)rcw;
            Guid iid = IID_IDXGISwapChain1;
            int hr = wrap.GetNativeResource(IntPtr.Zero, 0f, in iid, out IntPtr resource);
            Marshal.ReleaseComObject(rcw);
            if (hr < 0) Marshal.ThrowExceptionForHR(hr);
            return resource;
        }
        finally { Marshal.Release(insp); }
    }

    private const string WndClassName = "DontSpeakWinUIDictationPanel";

    private const uint WS_POPUP = 0x80000000;
    private const uint WS_EX_NOREDIRECTIONBITMAP = 0x00200000,
        WS_EX_NOACTIVATE = 0x08000000, WS_EX_TOPMOST = 0x00000008, WS_EX_TOOLWINDOW = 0x00000080;
    private const uint WM_NCHITTEST = 0x0084, WM_SETCURSOR = 0x0020, WM_SIZING = 0x0214,
        WM_ENTERSIZEMOVE = 0x0231, WM_EXITSIZEMOVE = 0x0232, WM_MOUSEACTIVATE = 0x0021,
        WM_SETTINGCHANGE = 0x001A;
    private const int HTTRANSPARENT = -1, HTCAPTION = 2, HTLEFT = 10, HTRIGHT = 11;
    private const int MA_NOACTIVATE = 3;
    private const int WMSZ_LEFT = 1, WMSZ_TOPLEFT = 4, WMSZ_BOTTOMLEFT = 7;
    private static readonly IntPtr IDC_SIZEALL = (IntPtr)32646, IDC_SIZEWE = (IntPtr)32644;
    private const uint SPI_GETWORKAREA = 0x0030;
    private const int SM_CXSCREEN = 0, SM_CYSCREEN = 1;
    private static readonly IntPtr HWND_TOPMOST = new(-1);
    private const uint SWP_NOSIZE = 0x0001, SWP_NOMOVE = 0x0002, SWP_NOZORDER = 0x0004,
        SWP_NOACTIVATE = 0x0010, SWP_SHOWWINDOW = 0x0040, SWP_HIDEWINDOW = 0x0080,
        SWP_ASYNCWINDOWPOS = 0x4000;

    // Win2D unwrap only; DComp/DXGI via Vortice. Shared class/DC in Win32.cs.
    private static readonly Guid IID_IDXGISwapChain1 = new("790a45f7-0d42-4876-983a-0a55cfe6f4aa");

    [StructLayout(LayoutKind.Sequential)]
    private struct RECT { public int left; public int top; public int right; public int bottom; }

    [DllImport("user32.dll")]
    private static extern bool GetWindowRect(IntPtr hwnd, out RECT rect);

    [DllImport("user32.dll")]
    private static extern bool SystemParametersInfoW(uint action, uint uiParam, ref RECT pvParam, uint winIni);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr LoadCursorW(IntPtr hInstance, IntPtr lpCursorName);

    [DllImport("user32.dll")]
    private static extern IntPtr SetCursor(IntPtr hCursor);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool SetWindowPos(IntPtr hwnd, IntPtr insertAfter, int x, int y, int cx, int cy, uint flags);

    [ComImport, Guid("5f10688d-ea55-4d55-a3b0-4ddb55c0c20a"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface ICanvasResourceWrapperNative
    {
        [PreserveSig] int GetNativeResource(IntPtr device, float dpi, in Guid iid, out IntPtr resource);
    }
}
