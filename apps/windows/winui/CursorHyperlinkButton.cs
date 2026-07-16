using Microsoft.UI.Input;
using Microsoft.UI.Xaml.Controls;

namespace DontSpeak;

/// <summary>
/// HyperlinkButton with a hover cursor (WinUI 3 links have none). Default Hand matches macOS
/// <c>LinkCursorOnHover</c>. Cursor is on <c>UIElement.ProtectedCursor</c> — only settable from a
/// subclass, and only after the visual tree loads, so this sets it in Loaded.
/// </summary>
internal sealed partial class CursorHyperlinkButton : HyperlinkButton
{
    public InputSystemCursorShape CursorShape { get; set; } = InputSystemCursorShape.Hand;

    public CursorHyperlinkButton()
    {
        Loaded += (_, _) => ProtectedCursor = InputSystemCursor.Create(CursorShape);
    }
}
