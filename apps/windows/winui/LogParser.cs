using System;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace DontSpeak;

/// <summary>One combined-activity-log line for the Logs tab.</summary>
internal readonly record struct LogLine(string Source, string Level, string Text);

/// <summary>Logs tab JSON parse/filter — pure, no WinAppRuntime (unit-testable).</summary>
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

    /// <summary>See <c>ds_log::distinct_sources</c>.</summary>
    internal static List<string> DistinctSources(IReadOnlyList<LogLine> lines)
    {
        var ordered = new List<string>();
        var seen = new HashSet<string>(StringComparer.Ordinal);
        foreach (var l in lines)
        {
            if (l.Source.Length == 0 || !seen.Add(l.Source)) continue;
            ordered.Add(l.Source);
        }
        return ordered;
    }

    /// <summary>See <c>ds_log::filter_logs</c>.</summary>
    internal static List<LogLine> Filter(IReadOnlyList<LogLine> lines, string query)
    {
        var q = (query ?? "").Trim();
        if (q.Length == 0) return lines.ToList();
        return lines.Where(l =>
            l.Text.Contains(q, StringComparison.OrdinalIgnoreCase)
            || l.Source.Contains(q, StringComparison.OrdinalIgnoreCase)
            || l.Level.Contains(q, StringComparison.OrdinalIgnoreCase)).ToList();
    }

    private sealed record LogLineRaw(
        [property: JsonPropertyName("source")] string? Source,
        [property: JsonPropertyName("level")] string? Level,
        [property: JsonPropertyName("text")] string? Text);
}
