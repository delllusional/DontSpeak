using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text.Json;

namespace DontSpeak;

/// <summary>
/// The shared localization catalog (ds-i18n) over the same C ABI the macOS app uses —
/// ONE catalog rendered by every platform. English is the fallback; the active locale
/// defaults to the OS language (resolved lazily in Rust). XAML pulls strings via the
/// <c>{loc:Loc Key=...}</c> markup extension; code-behind calls <see cref="T(string)"/>.
/// </summary>
internal static class Loc
{
    private const string Dll = "ds_core.dll";

    [DllImport(Dll)] private static extern IntPtr ds_t([MarshalAs(UnmanagedType.LPUTF8Str)] string key);
    [DllImport(Dll)] private static extern IntPtr ds_t_args([MarshalAs(UnmanagedType.LPUTF8Str)] string key,
                                                                  [MarshalAs(UnmanagedType.LPUTF8Str)] string argsJson);
    [DllImport(Dll)] private static extern void ds_string_free(IntPtr s);

    /// <summary>Localized string for <paramref name="key"/> (English fallback; a missing key returns the key).</summary>
    public static string T(string key) => Take(ds_t(key));

    /// <summary>Localized string with <c>%{name}</c> placeholders filled from <paramref name="args"/>.
    /// Numbers should be formatted by the caller (culture-aware) and passed as strings.</summary>
    public static string T(string key, IReadOnlyDictionary<string, string> args)
        => Take(ds_t_args(key, JsonSerializer.Serialize(args)));

    private static string Take(IntPtr ptr)
    {
        if (ptr == IntPtr.Zero) return "";
        try { return Marshal.PtrToStringUTF8(ptr) ?? ""; }
        finally { ds_string_free(ptr); }
    }
}
