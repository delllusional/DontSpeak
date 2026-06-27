using Microsoft.UI.Input;
using Microsoft.UI.Xaml.Controls;

namespace DontSpeak;

/// <summary>
/// A <see cref="HyperlinkButton"/> that sets a hover cursor (WinUI 3 gives links no cursor of
/// their own). <see cref="CursorShape"/> picks which: the default <c>Wait</c> spinner — the
/// Windows analog of the macOS version link's colorful spinning beachball — for the version
/// link, or <c>Hand</c> for a normal "opens a page" link. The cursor lives in
/// <c>UIElement.ProtectedCursor</c>, which is only settable from a subclass and not before the
/// visual tree loads, hence this control sets it in Loaded.
/// </summary>
internal sealed partial class CursorHyperlinkButton : HyperlinkButton
{
    /// <summary>The hover cursor shape. Default <see cref="InputSystemCursorShape.Wait"/> (the
    /// version link); set to <see cref="InputSystemCursorShape.Hand"/> for a normal link.</summary>
    public InputSystemCursorShape CursorShape { get; set; } = InputSystemCursorShape.Wait;

    public CursorHyperlinkButton()
    {
        Loaded += (_, _) => ProtectedCursor = InputSystemCursor.Create(CursorShape);
    }
}
