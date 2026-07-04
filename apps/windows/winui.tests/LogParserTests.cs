using Xunit;

namespace DontSpeak.Tests;

/// <summary>
/// <see cref="LogParser.ParseLogs"/> — the combined activity log's JSON→<see cref="LogLine"/>
/// parse, exercised with canned JSON (the same wire shape <c>Native.LogsJson</c> returns)
/// instead of a real log file, mirroring <see cref="HealthSnapshotTests"/>.
/// </summary>
public class LogParserTests
{
    // ── Empty/malformed payloads must yield an empty list, never throw ──

    [Theory]
    [InlineData("")]
    [InlineData("   ")]
    [InlineData("not json at all")]
    [InlineData("{}")]
    [InlineData("[1,2,3]")]
    public void EmptyOrMalformedPayloadIsAnEmptyList(string json)
    {
        Assert.Empty(LogParser.ParseLogs(json));
    }

    // ── Happy path: every line's source/level/text is mapped in order ──

    [Fact]
    public void WellFormedPayloadMapsEveryLineInOrder()
    {
        var lines = LogParser.ParseLogs("""
            [
                {"source": "dontspeakd", "level": "INFO", "text": "engine started"},
                {"source": "ds-helper", "level": "ERROR", "text": "tts spawn failed"}
            ]
            """);
        Assert.Equal(2, lines.Count);
        Assert.Equal(new LogLine("dontspeakd", "INFO", "engine started"), lines[0]);
        Assert.Equal(new LogLine("ds-helper", "ERROR", "tts spawn failed"), lines[1]);
    }

    /// <summary>A missing/null field on a line reads as "" rather than null or throwing — the
    /// renderer (<c>MainWindow.RenderLogLines</c>) assumes non-null <see cref="LogLine"/> fields.</summary>
    [Fact]
    public void MissingOrNullFieldsDefaultToEmptyString()
    {
        var lines = LogParser.ParseLogs("""[{"text": "no source or level"}]""");
        var line = Assert.Single(lines);
        Assert.Equal("", line.Source);
        Assert.Equal("", line.Level);
        Assert.Equal("no source or level", line.Text);
    }

    [Fact]
    public void EmptyArrayIsAnEmptyList()
    {
        Assert.Empty(LogParser.ParseLogs("[]"));
    }

    /// <summary>Property names are matched case-insensitively, matching every other JSON
    /// boundary in this app (<c>ToolsJsonOptions</c>/<c>ModelStatusJsonOptions</c>).</summary>
    [Fact]
    public void PropertyNamesAreCaseInsensitive()
    {
        var lines = LogParser.ParseLogs("""[{"Source": "x", "LEVEL": "WARN", "Text": "hi"}]""");
        var line = Assert.Single(lines);
        Assert.Equal(new LogLine("x", "WARN", "hi"), line);
    }
}
