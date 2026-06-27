using Microsoft.UI.Xaml.Markup;

namespace DontSpeak;

/// <summary>
/// XAML markup extension that pulls a string from the shared catalog at load time:
/// <c>Text="{loc:Loc Key=status.caps_lock}"</c> → <see cref="Loc.T(string)"/>.
/// Declare <c>xmlns:loc="using:DontSpeak"</c>. Resolved once at load; strings that change
/// with locale at runtime are set from code-behind instead.
/// </summary>
[MarkupExtensionReturnType(ReturnType = typeof(string))]
internal sealed partial class LocExtension : MarkupExtension
{
    public string Key { get; set; } = "";

    protected override object ProvideValue() => Loc.T(Key);
}
