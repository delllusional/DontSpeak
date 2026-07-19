using Microsoft.UI.Xaml.Markup;

namespace DontSpeak;

/// <summary>
/// Load-time catalog lookup: <c>Text="{loc:Loc Key=status.caps_lock}"</c> → <see cref="Loc.T(string)"/>.
/// <c>xmlns:loc="using:DontSpeak"</c>. Runtime-locale strings come from code-behind.
/// </summary>
[MarkupExtensionReturnType(ReturnType = typeof(string))]
internal sealed partial class LocExtension : MarkupExtension
{
    public string Key { get; set; } = "";

    protected override object ProvideValue() => Loc.T(Key);
}
