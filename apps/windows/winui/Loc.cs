using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text.Json;

namespace DontSpeak;

/// <summary>
/// Shared ds-i18n catalog over the same C ABI as macOS. English fallback; locale defaults to OS
/// language (resolved in Rust). XAML: <c>{loc:Loc Key=...}</c>; code: <see cref="T(string)"/>.
/// </summary>
internal static class Loc
{
    private const string Dll = "ds_core.dll";

    [DllImport(Dll)] private static extern IntPtr ds_t([MarshalAs(UnmanagedType.LPUTF8Str)] string key);
    [DllImport(Dll)] private static extern IntPtr ds_t_args([MarshalAs(UnmanagedType.LPUTF8Str)] string key,
                                                                  [MarshalAs(UnmanagedType.LPUTF8Str)] string argsJson);

    /// <summary>Catalog lookup; missing key returns the key. English fallback.</summary>
    public static string T(string key) => Native.TakeString(ds_t(key));

    /// <summary>Lookup with <c>%{name}</c> placeholders from <paramref name="args"/>.
    /// Caller formats numbers (culture-aware) as strings.</summary>
    public static string T(string key, IReadOnlyDictionary<string, string> args)
        => Native.TakeString(ds_t_args(key, JsonSerializer.Serialize(args)));
}
