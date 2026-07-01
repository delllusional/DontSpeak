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
/// The dictation transcript overlay — the Windows analogue of the macOS
/// <c>OverlayPanel</c>. A borderless, NON-ACTIVATING, always-on-top window that floats the
/// live/final transcript near the bottom of the screen while you dictate, without ever
/// stealing focus (you keep typing into whatever app is frontmost). It is DRAGGABLE: the
/// whole card is a drag handle, so you can move it out of the way, and it stays where you
/// put it for the session.
///
/// PRESENTATION — fully GPU, zero readback. Each frame is rasterized on the GPU with Win2D
/// (Direct2D) straight into a <see cref="CanvasSwapChain"/> back buffer (premultiplied BGRA),
/// and the swap chain is hosted on this window through <b>DirectComposition</b>
/// (<c>IDCompositionDevice</c> → <c>CreateTargetForHwnd</c> → a visual whose content IS the
/// swap chain). The DWM composition engine then blends the premultiplied-alpha content against
/// the desktop on the GPU. There is NO <c>UpdateLayeredWindow</c>, no GPU→CPU pixel readback,
/// no per-frame DIB, and no GDI round-trip — the whole pipeline stays on the GPU and is
/// vsync-paced, which is what makes the breathing glow and per-word blur-fades smooth.
///
/// ATOMIC RESIZE — a FIXED-SIZE container. The HWND and the swap chain are sized ONCE to a
/// fixed maximum height (<see cref="_maxSurfaceH"/>, enough for many transcript lines); they
/// do NOT resize as the transcript wraps to more lines. The card is drawn TOP-anchored inside
/// that fixed surface and grows DOWNWARD into the (transparent) lower region. Because the
/// window never changes size while streaming, there is no window/content size mismatch and so
/// NO 1-frame clip/flicker on a line-wrap — the property the old <c>UpdateLayeredWindow</c>
/// path got for free (position+size+pixels atomic), recovered here without the readback. The
/// empty region below the card is fully transparent and returns <c>HTTRANSPARENT</c> from the
/// hit-test, so clicks there fall through to whatever is underneath. <c>SetWindowPos</c> fires
/// only on first show, a user drag, or a horizontal width change — never per transcript line.
///
/// Why DirectComposition and not the WinUI compositor: a focus-safe overlay must be shown
/// WITHOUT activating (so the paste target keeps focus); a WinUI window can't guarantee "never
/// activate", and the LIFTED Windows App SDK compositor exposes no
/// <c>ICompositorDesktopInterop</c> to hang a visual tree on a bare HWND. Raw Win32
/// DirectComposition is a SEPARATE API with none of those limits: it binds a visual tree to any
/// HWND we own via <c>CreateTargetForHwnd</c>. So this stays a pure Win32 window
/// (<c>WS_EX_NOREDIRECTIONBITMAP | WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TOOLWINDOW</c>) with
/// a DirectComposition target. <c>WS_EX_NOREDIRECTIONBITMAP</c> drops the GDI redirection
/// surface so the per-pixel-alpha DComp content shows cleanly with no flash.
///
/// THREADING — one dedicated render thread. All GPU + DirectComposition work runs on a single
/// long-lived <see cref="_renderThread"/>, paced by the swap chain's vsync (<c>Present(1)</c>);
/// while nothing is animating it blocks on <see cref="_wake"/>. The caller's
/// <see cref="Update"/> (always on the UI thread — see App's status push) does NO drawing: it
/// publishes an immutable <see cref="Snapshot"/> and signals the thread. This replaced a
/// <c>System.Threading.Timer</c> render loop that ran on the shared .NET thread pool — that
/// pool is also hammered by the engine's STT/RPC work, so the animation callbacks were starved
/// (and unaligned to vblank) precisely while you dictated. A dedicated, vsync-paced thread
/// removes both the starvation and the judder. The render loop also SELF-HEALS: a device loss
/// OR any other render exception tears down and rebuilds the GPU+DComp stack (with backoff)
/// rather than killing the thread and freezing the overlay for the session. All cross-thread
/// window calls use <c>SWP_ASYNCWINDOWPOS</c> so the render thread never blocks on the UI
/// thread (which matters at teardown, when the UI thread is inside <c>Dispose</c>'s join).
///
/// Win11-native in look: a fixed-width rounded card with a translucent "glass" tint that
/// follows the system **dark/light** theme; while listening with nothing recognized yet it
/// shows a slow **breathing accent-color glow** (the neon-glow idiom); streaming words
/// **blur-fade in** per word, and a word that gets REFINED (e.g. "their"→"there")
/// **blur-replaces** — old glyphs blur out as the new ones blur in, at the same slot.
///
/// The caller drives it from its status poll via <see cref="Update"/>: show with the current
/// transcript while <c>awaiting_confirm || (recording &amp;&amp; local_stt)</c>, hide otherwise.
/// </summary>
internal sealed class DictationPanel : IDisposable
{
    private readonly WndProcDelegate _wndProc; // kept alive: a GC of this crashes the WndProc
    private readonly IntPtr _hwnd;
    private bool _disposed;
    private readonly Stopwatch _clock = Stopwatch.StartNew();

    // ── published state (UI thread writes, render thread reads) ──────────────────────────────
    // An immutable snapshot swapped atomically by Update/OnThemeChanged (both on the UI thread,
    // so they serialize) and read once per frame by the render thread — lock-free.
    private volatile Snapshot _snap = Snapshot.Empty;

    // Drag/resize state, written by WndProc (UI thread), read by the render thread when placing
    // the window. Plain volatile fields rather than part of the snapshot because they change on
    // the OS move/size loop, out of band with Update.
    private volatile bool _userMoved;     // the user dragged it → keep their position
    private volatile int _userPosX, _userPosY; // user-chosen top-left (screen px)
    private volatile bool _dragging;      // inside an OS move/size loop (WM_ENTER/EXITSIZEMOVE)
    private volatile int _userWidth;      // user-resized card width (0 = default CardWidth)
    // The card's bottom edge in WINDOW-local px, published by the render thread each frame and
    // read by WndProc's hit-test: anything below it (the transparent lower region of the
    // fixed-height container) is click-through. 0 until the first frame is drawn.
    private volatile int _cardBottomInWin;

    // ── render thread ────────────────────────────────────────────────────────────────────────
    private readonly Thread _renderThread;
    private readonly ManualResetEventSlim _wake = new(false); // set ⇒ render; reset ⇒ idle
    private volatile bool _stop;
    private int _renderFailures;            // consecutive non-device-lost render failures (render-thread-only)
    private const int MaxRenderFailures = 8; // after this many in a row, idle until the next signal rather than hot-loop

    /// <summary>An immutable per-render view of everything the frame needs. Built on the UI
    /// thread, read on the render thread; arrays are never mutated after publication.</summary>
    private sealed class Snapshot
    {
        public bool Visible;
        public bool GlowOn;                 // draw the glow (listening OR no paste target)
        public bool WholePill;              // true = wash the whole pill (no target); false = frame glow (speak-now)
        public bool Light;                  // system theme at publish time
        public WinColor Glow;               // glow color (always the dictation orange)
        public string[] Words = Array.Empty<string>();      // transcript tokens (per-word fade-in)
        public long[] AppearMs = Array.Empty<long>();       // each word's first-seen time (fade origin)
        public string?[] OutWords = Array.Empty<string?>(); // per-slot OUTGOING word fading out (or null)
        public long[] OutAppearMs = Array.Empty<long>();    // each outgoing word's fade-out start
        public static readonly Snapshot Empty = new();
    }

    // ── GPU / Win2D (render-thread-only — no locking) ────────────────────────────────────────
    private CanvasDevice? _device;          // shared Direct2D/Direct3D device
    private CanvasTextFormat? _fmt;          // Segoe UI transcript format
    private CanvasSwapChain? _swapChain;     // the on-GPU back buffer presented via DComp
    private int _swapW, _swapH;              // current swap-chain pixel size
    private IDXGISwapChain1? _dxgiSwap;       // Vortice wrapper over the native swap chain (owns the ref; DComp visual content)
    private float _lineH;                    // cached transcript line height (DIP/px @96)
    private int _maxSurfaceH;                // FIXED swap-chain / window height (set once per device)
    private readonly Dictionary<string, float> _wordW = new();              // word → advance width
    private readonly Dictionary<string, CanvasRenderTarget> _tiles = new(); // word → glyph tile (blur source)
    private WinColor _tileColor;             // theme color the cached tiles were drawn in
    // Cached baked glow tiles. The glow's SHAPE never changes while it breathes — only its
    // opacity — so the expensive Gaussian blurs are pre-rasterized ONCE per size into tiles, and
    // each frame just composites them at the breathing opacity. No per-frame Gaussian. The bake
    // only runs while the glow is actually on (see EnsureGlowResources call site).
    private CanvasRenderTarget? _outerGlowTile; // pre-blurred outer halo (surface-sized)
    private CanvasRenderTarget? _frameGlowTile; // pre-blurred + inset-clipped frame stroke
    private int _glowW, _glowH;               // card size the glow tiles were baked for

    // ── DirectComposition (render-thread-only) — Vortice.DirectComposition types ──────────────
    private IDCompositionDevice? _dcompDevice;
    private IDCompositionTarget? _dcompTarget;
    private IDCompositionVisual? _dcompVisual;

    // ── window placement cache (render-thread-only) ──────────────────────────────────────────
    private bool _shownNative;               // the show SetWindowPos has run
    private bool _wasDragging;               // last frame saw _dragging (to resync on drop)
    private int _curX, _curY, _curW, _curH;  // current HWND bounds (skip redundant SetWindowPos)

    // Layout (logical px == GPU px; the surface is built at 96 DPI so 1 unit == 1 pixel —
    // DPI scaling of the overlay is a refinement, not needed for parity).
    private const int CardWidth = 460;                          // matches the macOS 460-pt pill
    private const int MinCardWidth = 240, MaxCardWidth = 900;   // horizontal-resize bounds
    private const int PadX = 18, PadY = 13, Radius = 14;
    private const int BottomMargin = 90;     // gap above the taskbar, mirroring the macOS placement
    private const int GlowMargin = 26;       // slack around the card so the breathing glow has room
    private const int MaxExtraLines = 13;    // downward growth budget for the fixed-height container
                                             // (one line + this many = the tallest transcript shown
                                             // without the window resizing; ~14 lines is far beyond
                                             // any live partial — long enough that resize never flickers)
    // ── TUNE: blur-replace feel — raised from macOS parity for 60Hz; revisit vs a real macOS
    //    side-by-side recording. Original macOS-parity values in the comments below. ──────────
    private const float FadeMs = 360f;       // TUNE (macOS parity was 220f): longer fade so the
                                             // blur-replace reads clearly on a 60Hz panel (more frames)
    private const int BreathMs = 2400;       // breathing-glow cycle (macOS easeInOut(1.2) autoreverse)
    private const float FontSizeDip = 20f;   // 15pt == 20 DIP @96 (parity with the old GDI+ 15pt Segoe UI)
    private const float WordGap = 6f;        // inter-word advance (DWrite drops trailing space)
    private const float MaxBlur = 9f;        // TUNE (macOS parity was 6f): peak Gaussian blur (px) at the
                                             // start/end of a transition — bumped so it reads as a BLUR, not a fade
    private const float TilePad = 24f;       // TUNE (was 14f): padding around a glyph tile so the blur halo
                                             // isn't clipped — keep ≥ ~2.6·MaxBlur (raise this if MaxBlur grows)
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
        // No WS_EX_LAYERED: DirectComposition content can't share a window with the GDI layered
        // path, and WS_EX_NOREDIRECTIONBITMAP drops the redirection surface so the DComp
        // per-pixel-alpha content shows with no flash. No WS_EX_TRANSPARENT: the card is
        // DRAGGABLE (the drawn card is a caption — see WndProc), so it must receive the mouse;
        // WS_EX_NOACTIVATE still keeps a drag from stealing keyboard focus from the app you're
        // dictating into. Created zero-size + hidden; the render thread sizes/positions/shows it.
        _hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            WndClassName, "DontSpeak Dictation", WS_POPUP,
            0, 0, 0, 0, IntPtr.Zero, IntPtr.Zero, hinstance, IntPtr.Zero);

        _renderThread = new Thread(RenderLoop) { IsBackground = true, Name = "dictation-render" };
        _renderThread.Start();
    }

    /// <summary>Show the overlay with <paramref name="text"/> (or hide it). Called from the
    /// status poll on the UI thread. Publishes an immutable snapshot and wakes the render thread;
    /// it does NO drawing itself. While recording with no transcript yet it shows the animated
    /// "listening" state; once words arrive it shows the wrapped transcript.
    /// <paramref name="promptGlow"/> is the engine's shared "speak now" decision (the core's
    /// <c>prompt_glow</c>) — NOT recomputed here, so it can't drift from macOS.</summary>
    public void Update(bool visible, string text, bool hasTarget, bool promptGlow)
    {
        if (_disposed || _hwnd == IntPtr.Zero) return;
        if (!visible)
        {
            if (_snap.Visible) { _snap = new Snapshot(); _wake.Set(); } // Visible=false ⇒ render thread hides
            return;
        }

        var prev = _snap;
        bool hasText = !string.IsNullOrWhiteSpace(text);
        // The glow shows while the engine says "speak now" (empty pill prompting the user to
        // talk) OR whenever there's no editable paste target ("nowhere to submit this") — the
        // latter regardless of transcript text. BOTH are the SAME dictation orange; they differ
        // by SHAPE: speak-now glows the FRAME (edges); no paste target washes the WHOLE pill.
        var s = new Snapshot
        {
            Visible = true,
            GlowOn = promptGlow || !hasTarget,
            WholePill = !hasTarget,
            Light = IsLightTheme(),
            Glow = Brand.Warning, // always the dictation orange (shared Brand.warning)
        };
        BuildWords(prev, s, hasText ? text.Trim() : "");
        _snap = s;
        _wake.Set();
    }

    /// <summary>Diff the incoming transcript against the previous snapshot's words: NEW or
    /// changed words are stamped "now" (so they blur in); a word REPLACED at a slot is captured
    /// as the slot's OUTGOING word (so it blurs out while the replacement blurs in); unchanged
    /// leading words keep their original time so the stable prefix doesn't re-animate as partials
    /// stream — the Windows analogue of the macOS per-word `.blurReplace` keyed by position·word.
    /// Pure data, on the UI thread; no GPU access.</summary>
    private void BuildWords(Snapshot prev, Snapshot s, string text)
    {
        var nw = text.Length == 0
            ? Array.Empty<string>()
            : text.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries); // any whitespace
        var na = new long[nw.Length];
        var ow = new string?[nw.Length];
        var oa = new long[nw.Length];
        long now = _clock.ElapsedMilliseconds;
        for (int i = 0; i < nw.Length; i++)
        {
            bool unchanged = i < prev.Words.Length && prev.Words[i] == nw[i];
            na[i] = unchanged ? prev.AppearMs[i] : now;
            if (unchanged)
            {
                // Carry over an outgoing fade still in flight at this slot (rapid re-renders).
                if (i < prev.OutWords.Length && prev.OutWords[i] != null && now - prev.OutAppearMs[i] < FadeMs)
                {
                    ow[i] = prev.OutWords[i];
                    oa[i] = prev.OutAppearMs[i];
                }
            }
            else if (i < prev.Words.Length && !string.IsNullOrEmpty(prev.Words[i]))
            {
                // The previous word at this slot was REPLACED → blur it out at the same slot.
                ow[i] = prev.Words[i];
                oa[i] = now;
            }
        }
        s.Words = nw;
        s.AppearMs = na;
        s.OutWords = ow;
        s.OutAppearMs = oa;
    }

    /// <summary>React to a live system dark/light switch (WM_SETTINGCHANGE / ImmersiveColorSet):
    /// republish the current snapshot with the new theme and wake the render thread. The glyph
    /// tiles self-heal — <see cref="WordTile"/> drops them when the text color changes. Runs on
    /// the UI thread (WndProc).</summary>
    private void OnThemeChanged()
    {
        var prev = _snap;
        if (!prev.Visible) return;
        bool light = IsLightTheme();
        if (light == prev.Light) return; // an unrelated setting change → nothing to do
        _snap = new Snapshot
        {
            Visible = true,
            GlowOn = prev.GlowOn,
            WholePill = prev.WholePill,
            Light = light,
            Glow = prev.Glow,
            Words = prev.Words,
            AppearMs = prev.AppearMs,
            OutWords = prev.OutWords,
            OutAppearMs = prev.OutAppearMs,
        };
        _wake.Set();
    }

    // ── render thread ────────────────────────────────────────────────────────────────────────

    /// <summary>The sole render loop. Blocks on <see cref="_wake"/> while idle; once awake it
    /// renders one frame and, if anything is still animating (the listening glow, or a word in a
    /// blur transition), keeps looping — paced by the swap chain's vsync via <c>Present(1)</c>,
    /// not a timer. When the frame is static it resets <see cref="_wake"/> and sleeps until the
    /// next <see cref="Update"/>/theme/drag signal. SELF-HEALING: a device loss or ANY other
    /// render exception tears the GPU+DComp stack down and rebuilds it next iteration (with
    /// backoff), so a transient failure can't permanently freeze the overlay. All Win2D +
    /// DirectComposition objects live and die on THIS thread.</summary>
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
                        ClearTiles(); // free the transitioning-word glyph cache between dictations
                        _wordW.Clear(); // and the word-width cache — it grows per distinct word
                    }
                    else
                    {
                        animating = RenderOnce(s);
                    }
                    _renderFailures = 0; // a clean pass clears the failure streak
                }
                catch (Exception ex) when (_device != null && _device.IsDeviceLost(ex.HResult))
                {
                    // GPU device lost (driver reset / sleep): drop the whole GPU + DComp stack and
                    // rebuild from scratch next loop. Brief back-off so a persistent loss can't
                    // hot-spin while it rebuilds.
                    CleanupGpu();
                    _shownNative = false;
                    Thread.Sleep(200);
                    continue;
                }
                catch
                {
                    // ANY other render error (a transient D2D hiccup, an effect failure, OOM, a
                    // cross-thread placement race): do NOT let it kill the render thread and freeze
                    // the overlay for the rest of the session. Tear the stack down and rebuild next
                    // iteration, backing off proportionally. After a sustained streak, stop
                    // hot-looping and idle until the next state change wakes us to retry clean.
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

                if (animating && !_stop) continue; // keep rendering; Present(1) already paced us to vblank

                // Settled/hidden → sleep until the next signal. Reset THEN re-check the published
                // snapshot closes the lost-wake-up race where Update() publishes + signals between
                // our read of _snap above and this Reset (which would otherwise strand the new
                // state until the following event).
                _wake.Reset();
                if (ReferenceEquals(_snap, s) && !_stop) _wake.Wait();
            }
        }
        catch { /* teardown race — swallow */ }
        finally { CleanupGpu(); }
    }

    /// <summary>Render exactly one frame into the (fixed-height) swap chain and present it; size
    /// and position the window only on first show / move / width change. Returns true if the frame
    /// is still animating (caller keeps looping). A FIXED-WIDTH rounded glass card (macOS 460-pt
    /// parity), TOP-anchored inside the fixed-height surface so it grows downward: while listening
    /// with no transcript it's an empty card with a breathing accent glow; once words arrive it
    /// shows the wrapped transcript, refined words blur-replacing.</summary>
    private bool RenderOnce(Snapshot s)
    {
        EnsureDevice();

        long now = _clock.ElapsedMilliseconds;
        int cw = _userWidth > 0 ? Math.Clamp(_userWidth, MinCardWidth, MaxCardWidth) : CardWidth;
        int textArea = cw - PadX * 2;

        // Card content height = a word-wrap layout pass (one line tall when empty / listening).
        int th = (int)Math.Ceiling(LayoutWords(s, null, 0, 0, textArea, false, default, now));
        int cardW = cw, cardH = th + PadY * 2;
        int w = cardW + GlowMargin * 2;        // window/surface WIDTH (tracks the card width)
        int h = _maxSurfaceH;                  // window/surface HEIGHT — FIXED (never grows per line)
        int ox = GlowMargin, oy = GlowMargin;  // card origin inside the surface (top-anchored)
        // A one-line card's window height — the top-anchor reference so the first line is pinned
        // to the same screen Y regardless of how many lines the transcript currently has.
        int oneLineWinH = (int)Math.Ceiling(_lineH) + PadY * 2 + GlowMargin * 2;

        EnsureSwapChain(w, h);
        var cardRect = new Rect(ox, oy, cardW, cardH);

        // Breathing intensity (0..1) — slow ease-in-out via the sine; 0 when not listening.
        // Time-based (not per-frame accumulation) so it's identical at any refresh rate.
        double phase = (now % BreathMs) / (double)BreathMs;
        bool fading = AnyFading(s, now);
        bool animating = s.GlowOn || fading;
        double gi = s.GlowOn ? 0.45 + 0.55 * (Math.Sin(phase * 2 * Math.PI) * 0.5 + 0.5) : 0;

        // Bake the (size-dependent, opacity-independent) glow tiles ONLY while the glow is drawn —
        // during normal transcript streaming with a paste target the glow is off, so skip the two
        // Gaussian bakes entirely (they'd otherwise re-run on every line-wrap for nothing).
        if (s.GlowOn) EnsureGlowResources(cardW, cardH, cardRect, w, h, s.Glow);

        using (var ds = _swapChain!.CreateDrawingSession(Transparent))
        {
            // Neon glow: a REAL Gaussian-blurred halo fanning OUTWARD behind the card, then the
            // card, then either the whole-pill wash (no target) or the frame glow (speak-now),
            // then the transcript on top.
            if (s.GlowOn) DrawOuterGlow(ds, gi);
            DrawCard(ds, cardRect, s.Light);
            if (s.GlowOn && s.WholePill)
            {
                int a = (int)(255 * (0.16 + 0.26 * gi)); // breathing orange wash over the whole pill
                ds.FillRoundedRectangle(cardRect, Radius, Radius, Argb(a, s.Glow));
            }
            else if (s.GlowOn)
            {
                DrawFrameGlow(ds, cardRect, gi, s.Glow); // speak-now → blurred frame + crisp core ring
            }
            LayoutWords(s, ds, ox + PadX, oy + PadY, textArea, true, TextColor(s.Light), now);
        }
        _swapChain.Present(1); // vsync — smooth, no tearing; DWM blends via DComp on the GPU

        // Publish the card's bottom edge so the hit-test treats the transparent region below the
        // card (the fixed-height container's slack) as click-through.
        _cardBottomInWin = oy + cardH;

        PlaceWindow(w, h, oneLineWinH);
        return animating;
    }

    /// <summary>Is any word still inside a transition window (fading in, or fading out)?</summary>
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
        // The fixed container height: one line + the downward-growth budget. Computed once the
        // line height is known; constant for the device's lifetime so the swap chain + window
        // never resize as the transcript wraps (the source of the old 1-frame flicker).
        _maxSurfaceH = (int)Math.Ceiling(_lineH) + PadY * 2 + GlowMargin * 2
                       + MaxExtraLines * (int)Math.Ceiling(_lineH);
        _wordW.Clear();
        ClearTiles();
        _tileColor = default;
    }

    /// <summary>Create (or resize) the composition swap chain and, on first creation, bind it to
    /// the window through DirectComposition. ResizeBuffers keeps the SAME swap-chain object, so
    /// the DComp visual content stays bound — no re-commit needed on resize. Height is fixed
    /// (<see cref="_maxSurfaceH"/>), so ResizeBuffers fires only on a horizontal width change.</summary>
    private void EnsureSwapChain(int w, int h)
    {
        if (_swapChain == null)
        {
            // Premultiplied BGRA composition swap chain — exactly the format DWM expects for
            // per-pixel-alpha DirectComposition content.
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

    /// <summary>Stand up the DirectComposition tree: a device on the swap chain's GPU, a target
    /// bound to our HWND, and a visual whose content IS the swap chain. Done once per swap chain.
    /// </summary>
    private void SetupDComp()
    {
        // Unwrap Win2D's CanvasSwapChain to the native IDXGISwapChain1 (owned ref). The Vortice
        // wrapper takes ownership of that ref and releases it on Dispose (CleanupGpu). This same
        // object is both the DComp visual content and the handle we ask for the IDXGIDevice.
        IntPtr nativeSwap = GetNativeSwapChain(_swapChain!);
        _dxgiSwap = new IDXGISwapChain1(nativeSwap);

        // The DComp device must sit on the same GPU as the swap chain → get its IDXGIDevice.
        using IDXGIDevice dxgiDevice = _dxgiSwap.GetDevice<IDXGIDevice>();
        _dcompDevice = DComp.DCompositionCreateDevice<IDCompositionDevice>(dxgiDevice);

        // topmost=true: the visual composites above the (empty) window surface.
        _dcompDevice.CreateTargetForHwnd(_hwnd, true, out _dcompTarget).CheckError();
        _dcompVisual = _dcompDevice.CreateVisual();
        _dcompVisual.SetContent(_dxgiSwap);     // SetContent AddRefs the swap chain
        _dcompTarget.SetRoot(_dcompVisual);
        _dcompDevice.Commit().CheckError();
    }

    /// <summary>The advance width of <paramref name="word"/> (cached; width is theme-independent).</summary>
    private float WordWidth(string word)
    {
        if (_wordW.TryGetValue(word, out float w)) return w;
        using var layout = new CanvasTextLayout(_device, word, _fmt, 1e6f, 1e6f);
        w = (float)layout.LayoutBounds.Width;
        _wordW[word] = w;
        return w;
    }

    /// <summary>A GPU glyph tile for <paramref name="word"/> in <paramref name="color"/>, used as
    /// the blur source for a transitioning word. The glyph is drawn at (TilePad, TilePad) so the
    /// Gaussian halo has room. Cached; dropped when the theme color changes.</summary>
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

    /// <summary>Bake the (size-dependent, opacity-independent) blurred glow into tiles whenever
    /// the card size changes — the glow color is constant, so size is the only key. The two real
    /// Gaussian blurs run HERE, once per size, into surface-sized render targets; the breath then
    /// just composites these tiles at varying opacity (DrawOuterGlow/DrawFrameGlow) with no
    /// per-frame blur. This is what takes the animating render thread from ~⅔ of a core down to
    /// a sliver: a breath frame is now a couple of textured quads, not two Gaussians.</summary>
    private void EnsureGlowResources(int cardW, int cardH, Rect card, int surfW, int surfH, WinColor glow)
    {
        if (_outerGlowTile != null && _glowW == cardW && _glowH == cardH) return;
        _outerGlowTile?.Dispose();
        _frameGlowTile?.Dispose();

        // Outer halo: a 14px-blurred filled rounded rect, baked into a surface-sized tile at the
        // card's position so a later DrawImage at (0,0) reproduces the original pixels exactly.
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

        // Frame glow: a 6px-blurred stroke CLIPPED to the card interior (the inset half), baked.
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

    /// <summary>Lay out the transcript word-by-word with wrapping. When drawing, paint each word:
    /// stable words sharp; a word inside its fade window blur-fades IN (blur N→0, opacity 0→1),
    /// and any OUTGOING word at the slot blur-fades OUT at the same position — the GPU equivalent
    /// of the macOS per-word `.blurReplace`. Returns the laid-out height (one line when empty).
    /// </summary>
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

    /// <summary>Draw slot <paramref name="i"/> at (x,y): the outgoing word blurring out (if any)
    /// under the current word, which is either sharp (settled) or blurring in.</summary>
    private void DrawWordAt(Snapshot s, CanvasDrawingSession ds, int i, float x, float y, WinColor baseColor, long now)
    {
        // Outgoing (being replaced): blur 0→Max, opacity 1→0.
        if (i < s.OutWords.Length && s.OutWords[i] is string outW)
        {
            float q = Math.Clamp((now - s.OutAppearMs[i]) / FadeMs, 0f, 1f);
            if (q < 1f)
            {
                float qe = Ease(q);
                DrawTile(ds, outW, x, y, baseColor, qe * MaxBlur, 1f - qe);
            }
        }
        // Incoming / current: sharp once settled, else blur Max→0, opacity 0→1.
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

    /// <summary>Composite a glyph tile at (x,y) through a GPU Gaussian blur + opacity. A FRESH
    /// effect pair PER CALL is required: within one frame <see cref="DrawWordAt"/> draws BOTH an
    /// outgoing and an incoming tile, and Direct2D realizes a drawing session's effect graph
    /// lazily — reusing one mutable effect across the two <c>DrawImage</c> calls makes both render
    /// with the LAST-set parameters, so the outgoing blur-out is lost and a replace collapses into
    /// a plain swap (no visible blur). D2D refcounts the effect, so disposing right after DrawImage
    /// is safe. The glyph sits at (TilePad,TilePad) in the tile, so it's drawn offset back to land
    /// at (x,y).</summary>
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

    /// <summary>Outer half of the neon glow: the pre-blurred halo tile fanning OUTWARD behind the
    /// card (the box-shadow halo), composited at the breathing opacity. Drawn before the card so
    /// only the ring outside it shows through. Uses DrawImage's opacity overload (no per-frame
    /// effect-graph rebind).</summary>
    private void DrawOuterGlow(CanvasDrawingSession ds, double intensity)
    {
        var r = new Rect(0, 0, _outerGlowTile!.Size.Width, _outerGlowTile.Size.Height);
        ds.DrawImage(_outerGlowTile, r, r, (float)Math.Clamp(intensity * 0.45, 0, 1));
    }

    /// <summary>Inner half of the neon glow + the bright core ring: the pre-blurred, inset-clipped
    /// frame-stroke tile (light decaying inward, the box-shadow `inset` equivalent) at the
    /// breathing opacity, then a crisp core ring on the border. Together with
    /// <see cref="DrawOuterGlow"/> the edge glows on both sides, like the macOS blurred
    /// strokeBorder. Drawn on top of the card.</summary>
    private void DrawFrameGlow(CanvasDrawingSession ds, Rect card, double intensity, WinColor glow)
    {
        var r = new Rect(0, 0, _frameGlowTile!.Size.Width, _frameGlowTile.Size.Height);
        ds.DrawImage(_frameGlowTile, r, r, (float)Math.Clamp(intensity * 0.9, 0, 1));
        // Bright core ring on the border itself (the strokeBorder anchor) — crisp, so kept
        // per-frame for its breathing alpha; a single cheap stroke.
        ds.DrawRoundedRectangle(card, Radius, Radius, Argb((int)Math.Clamp(intensity * 140, 0, 255), glow), 1.4f);
    }

    /// <summary>Draw the rounded theme-tinted card (near-opaque, subtle 1px border).</summary>
    private static void DrawCard(CanvasDrawingSession ds, Rect rect, bool light)
    {
        // Translucent "glass" tint — the Win11 acrylic baseline is ~0.8 tint opacity. A composed
        // window can't do a live backdrop blur here, so this is a flat translucent tint (the
        // desktop shows faintly through); kept ≥0.8 so text stays legible over a busy background.
        WinColor bg = light ? WinColor.FromArgb(210, 244, 244, 247) : WinColor.FromArgb(204, 28, 28, 32);
        ds.FillRoundedRectangle(rect, Radius, Radius, bg);
        WinColor border = light ? WinColor.FromArgb(30, 0, 0, 0) : WinColor.FromArgb(34, 255, 255, 255);
        ds.DrawRoundedRectangle(rect, Radius, Radius, border, 1f);
    }

    private static WinColor TextColor(bool light) =>
        light ? WinColor.FromArgb(255, 24, 24, 28) : WinColor.FromArgb(255, 240, 240, 245);

    /// <summary><paramref name="c"/> at alpha <paramref name="a"/> (0..255, clamped).</summary>
    private static WinColor Argb(int a, WinColor c) => WinColor.FromArgb((byte)Math.Clamp(a, 0, 255), c.R, c.G, c.B);

    /// <summary>System app theme: HKCU Personalize\AppsUseLightTheme (1 = light). Re-read each
    /// Update so a live theme switch is reflected on the next frame.</summary>
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

    /// <summary>Position the fixed-size HWND. Unlike the old per-line resize, the window size is
    /// constant while streaming (width tracks the card; height is the fixed container), so
    /// <see cref="SetWindowPos"/> fires only on first show, a user move, or a width change.
    /// Bottom-center of the work area by default, TOP-anchored so the first line never moves and
    /// extra lines grow downward into the transparent slack; the user's dropped position once
    /// dragged. Cross-thread (render thread → UI-thread-owned HWND), so SWP_ASYNCWINDOWPOS posts
    /// the request instead of blocking on the UI thread — important at teardown.</summary>
    private void PlaceWindow(int w, int h, int oneLineWinH)
    {
        if (_dragging)
        {
            _wasDragging = true; // OS owns the bounds during a move/size loop — don't fight it
            return;
        }
        if (_wasDragging)
        {
            // Just dropped: resync our cache to where the OS left the window so we don't snap it.
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
            y = wa.bottom - oneLineWinH - BottomMargin; // top-anchored: a one-line card sits here
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

    /// <summary>Hide the window. Async (SWP_HIDEWINDOW + SWP_ASYNCWINDOWPOS) so this cross-thread
    /// call posts to the UI thread instead of blocking the render thread on it.</summary>
    private void HideWindow() =>
        SetWindowPos(_hwnd, IntPtr.Zero, 0, 0, 0, 0,
            SWP_HIDEWINDOW | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS);

    private static void GetWorkArea(out RECT rect)
    {
        rect = default;
        if (!SystemParametersInfoW(SPI_GETWORKAREA, 0, ref rect, 0))
        {
            // Fallback to the full primary screen if the query fails.
            rect = new RECT { left = 0, top = 0, right = GetSystemMetrics(SM_CXSCREEN), bottom = GetSystemMetrics(SM_CYSCREEN) };
        }
    }

    // The drawn card is a caption, so a left-drag on it moves the window; the dropped position is
    // captured (WM_EXITSIZEMOVE) and reused by PlaceWindow so the render loop never snaps it back.
    // The transparent region below the card (the fixed-height container's slack) is click-through.
    private IntPtr WndProc(IntPtr hwnd, uint msg, IntPtr wparam, IntPtr lparam)
    {
        switch (msg)
        {
            case WM_MOUSEACTIVATE:
                // Belt-and-braces with WS_EX_NOACTIVATE: a click/drag on the bar must NOT activate
                // it or pull keyboard focus from the app you're dictating into.
                return (IntPtr)MA_NOACTIVATE;
            case WM_SETTINGCHANGE:
                // A live system dark/light switch broadcasts WM_SETTINGCHANGE with lParam =
                // "ImmersiveColorSet" to every top-level window. The status push only re-Update()s
                // on an engine status-seq bump (a theme flip doesn't touch that), so without this
                // the card would keep its stale theme until the next hide/show.
                if (lparam != IntPtr.Zero && Marshal.PtrToStringUni(lparam) == "ImmersiveColorSet")
                    OnThemeChanged();
                break;
            case WM_NCHITTEST:
            {
                // The window is a fixed-height container; only the DRAWN card is interactive. The
                // empty glow padding above the card and the transparent slack below it (where the
                // transcript hasn't grown to yet) return HTTRANSPARENT so clicks fall through.
                // ToInt32() would throw OverflowException when bit 31 is set (negative screen
                // coords on a monitor left of/above the primary, zero-extended into the 64-bit
                // LPARAM) — and an exception escaping this WndProc kills the process.
                int lp = unchecked((int)lparam.ToInt64());
                int sx = (short)(lp & 0xFFFF);
                int sy = (short)((lp >> 16) & 0xFFFF);
                if (GetWindowRect(hwnd, out RECT wr))
                {
                    int relx = sx - wr.left, rely = sy - wr.top, width = wr.right - wr.left;
                    int cardBottom = _cardBottomInWin;
                    if (rely < GlowMargin || (cardBottom > 0 && rely > cardBottom))
                        return (IntPtr)HTTRANSPARENT;
                    // Left/right edge strips resize horizontally; the rest drags the card.
                    const int grip = GlowMargin + 10;
                    if (relx <= grip) return (IntPtr)HTLEFT;
                    if (relx >= width - grip) return (IntPtr)HTRIGHT;
                }
                return (IntPtr)HTCAPTION;
            }
            case WM_SETCURSOR:
            {
                int ht = (int)(lparam.ToInt64() & 0xFFFF); // low word = hit-test result
                if (ht == HTLEFT || ht == HTRIGHT) { SetCursor(LoadCursorW(IntPtr.Zero, IDC_SIZEWE)); return (IntPtr)1; }
                if (ht == HTCAPTION) { SetCursor(LoadCursorW(IntPtr.Zero, IDC_SIZEALL)); return (IntPtr)1; }
                break; // transparent/other → let DefWindowProc use the default arrow
            }
            case WM_SIZING:
            {
                // Horizontal-only resize: clamp the proposed width, write it back, remember it so
                // the render uses it (text re-wraps to the new width), and wake the render thread.
                // Height stays the fixed container height (the render loop owns h).
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
                // Pin wherever the user dropped it (covers both move and resize), then repaint.
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

    /// <summary>Tear down every GPU + DirectComposition resource. Runs on the render thread (the
    /// thread that created them) — on device-lost, on a general render failure, and at the end of
    /// the render loop. The CanvasDevice is shared, so it's only dropped (not disposed).</summary>
    private void CleanupGpu()
    {
        // Dispose in reverse dependency order. Vortice ComObjects Release their native ref on
        // Dispose; the visual holds its own ref on the swap chain (from SetContent), so the
        // order visual → target → device → swap keeps every refcount balanced.
        try { _dcompVisual?.Dispose(); } catch { } _dcompVisual = null;
        try { _dcompTarget?.Dispose(); } catch { } _dcompTarget = null;
        try { _dcompDevice?.Dispose(); } catch { } _dcompDevice = null;
        try { _dxgiSwap?.Dispose(); } catch { } _dxgiSwap = null; // releases the native swap-chain ref
        _swapChain?.Dispose(); _swapChain = null; _swapW = _swapH = 0;
        ClearTiles();
        _outerGlowTile?.Dispose(); _outerGlowTile = null;
        _frameGlowTile?.Dispose(); _frameGlowTile = null;
        _glowW = _glowH = 0;
        _fmt?.Dispose(); _fmt = null;
        _device = null; // shared — don't dispose
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        _stop = true;
        _wake.Set();
        // The render thread owns the GPU + DComp objects and frees them in its finally; join so
        // that teardown completes before we destroy the window. Bounded so a stuck frame can't
        // hang exit. The render thread's window calls are SWP_ASYNCWINDOWPOS (non-blocking
        // cross-thread), so it won't deadlock against this join even though the UI thread (here)
        // stops pumping while it waits.
        try { _renderThread.Join(1500); } catch { }
        _wake.Dispose();
        if (_hwnd != IntPtr.Zero) DestroyWindow(_hwnd);
        UnregisterClassW(WndClassName, GetModuleHandleW(null));
    }

    /// <summary>Unwrap Win2D's projected <see cref="CanvasSwapChain"/> to its native
    /// IDXGISwapChain1*. CsWinRT projects Win2D, so we take the object's native IInspectable, make
    /// a classic RCW, QI the documented <c>ICanvasResourceWrapperNative</c>, and ask it for the
    /// DXGI swap chain. Returns an owned ref (released in <see cref="CleanupGpu"/>).</summary>
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

    // ── constants ────────────────────────────────────────────────────────────────────────────
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

    // The only COM IID still needed: the Win2D native-unwrap shim asks CanvasSwapChain for its
    // IDXGISwapChain1 by IID. The DComp/DXGI interfaces themselves now come from Vortice.
    private static readonly Guid IID_IDXGISwapChain1 = new("790a45f7-0d42-4876-983a-0a55cfe6f4aa");

    // ── interop ──────────────────────────────────────────────────────────────────────────────
    // Shared window-class/DC bits come from Win32.cs (`using static`). DirectComposition + DXGI
    // are Vortice bindings; only the Win2D native-unwrap shim and the drag/theme/placement
    // P/Invokes specific to this overlay live here.
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

    // Win2D's native unwrap shim (Microsoft.Graphics.Canvas.native.h).
    [ComImport, Guid("5f10688d-ea55-4d55-a3b0-4ddb55c0c20a"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface ICanvasResourceWrapperNative
    {
        [PreserveSig] int GetNativeResource(IntPtr device, float dpi, in Guid iid, out IntPtr resource);
    }
}
