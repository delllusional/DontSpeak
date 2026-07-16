using System.Collections.Generic;
using System.Linq;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace DontSpeak;

/// <summary>One combined-activity-log line for the Logs tab.</summary>
internal readonly record struct LogLine(string Source, string Level, string Text);

/// <summary>Pure JSON→<see cref="LogLine"/> for the Logs tab wire shape (<c>Native.LogsJson</c>).
/// Standalone (not on <see cref="MainWindow"/>) so tests run without Windows App Runtime, like
/// <see cref="HealthSnapshot.FromJson(string, System.Func{string, double, string, string})"/>.</summary>
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
