using System.Collections.Generic;
using System.Linq;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace DontSpeak;

/// <summary>One line of the combined activity log — <see cref="LogParser.ParseLogs"/>'s
/// display-ready output (source tag, level token, message text).</summary>
internal readonly record struct LogLine(string Source, string Level, string Text);

/// <summary>Pure JSON→<see cref="LogLine"/> parse for the Logs tab (the wire shape
/// <c>Native.LogsJson</c>/dontspeakd's combined-log endpoint emits). Deliberately a
/// standalone class (not a member of <see cref="MainWindow"/>, which derives from the WinUI
/// <c>Window</c> type) so <c>DontSpeak.WinUI.Tests</c> can exercise it on a bare runner with no
/// Windows App Runtime, exactly like <see cref="HealthSnapshot.FromJson(string, System.Func{string, double, string, string})"/>.</summary>
internal static class LogParser
{
    private static readonly JsonSerializerOptions Options = new() { PropertyNameCaseInsensitive = true };

    internal static List<LogLine> ParseLogs(string json)
    {
        if (string.IsNullOrWhiteSpace(json)) return new();
        try
        {
            var raw = JsonSerializer.Deserialize<List<LogLineRaw>>(json, Options);
            return raw?.Select(d => new LogLine(d.Source ?? "", d.Level ?? "", d.Text ?? "")).ToList() ?? new();
        }
        catch { return new(); }
    }

    private sealed record LogLineRaw(
        [property: JsonPropertyName("source")] string? Source,
        [property: JsonPropertyName("level")] string? Level,
        [property: JsonPropertyName("text")] string? Text);
}
