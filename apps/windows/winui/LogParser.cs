using System;
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

    /// <summary>Distinct non-empty sources in first-appearance order (palette index). Lockstep
    /// with <c>ds_log::distinct_sources</c>.</summary>
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

    /// <summary>Case-insensitive substring over text, source, OR level. Blank query keeps all.
    /// Lockstep with <c>ds_log::filter_logs</c> (macOS LogCatalog / Linux filter).</summary>
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
