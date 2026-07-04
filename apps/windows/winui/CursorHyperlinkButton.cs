using Microsoft.UI.Input;
using Microsoft.UI.Xaml.Controls;

namespace DontSpeak;

/// <summary>
/// A <see cref="HyperlinkButton"/> that sets a hover cursor (WinUI 3 gives links no cursor of
/// their own). <see cref="CursorShape"/> defaults to <c>Hand</c> — the standard Windows "this
/// opens a link" cursor, matching the macOS pointing-hand cursor on the same version link
/// (<c>LinkCursorOnHover</c> in StatusView.swift). The cursor lives in
/// <c>UIElement.ProtectedCursor</c>, which is only settable from a subclass and not before the
/// visual tree loads, hence this control sets it in Loaded.
/// </summary>
internal sealed partial class CursorHyperlinkButton : HyperlinkButton
{
    /// <summary>The hover cursor shape. Default <see cref="InputSystemCursorShape.Hand"/>.</summary>
    public InputSystemCursorShape CursorShape { get; set; } = InputSystemCursorShape.Hand;

    public CursorHyperlinkButton()
    {
        Loaded += (_, _) => ProtectedCursor = InputSystemCursor.Create(CursorShape);
    }
}
