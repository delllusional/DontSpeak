using Xunit;

namespace DontSpeak.Tests;

/// <summary>
/// <see cref="LogParser.ParseLogs"/> with canned LogsJson-shaped JSON (no real log file).
/// </summary>
public class LogParserTests
{
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

    // Filter / distinct sources — lockstep with ds_log::catalog (Rust) and macOS LogCatalog.

    private static readonly LogLine[] Sample =
    {
        new("tts", "INFO", "spoke a sentence"),
        new("stt", "ERROR", "mic blocked"),
        new("caps", "WARN", "held too long"),
    };

    private static readonly string[] ExpectedSources = ["engine", "tts", "caps"];

    [Fact]
    public void DistinctSourcesPreserveFirstAppearance()
    {
        var lines = new[]
        {
            new LogLine("engine", "INFO", "a"),
            new LogLine("tts", "INFO", "b"),
            new LogLine("engine", "WARN", "c"),
            new LogLine("caps", "INFO", "d"),
            new LogLine("", "INFO", "skip"),
        };
        Assert.Equal(ExpectedSources, LogParser.DistinctSources(lines));
    }

    [Theory]
    [InlineData("")]
    [InlineData("   \t ")]
    public void FilterBlankKeepsAll(string query)
    {
        Assert.Equal(Sample.Length, LogParser.Filter(Sample, query).Count);
    }

    [Theory]
    [InlineData("BLOCKED", "stt")]
    [InlineData("caps", "caps")]
    [InlineData("error", "stt")]
    [InlineData("  stt  ", "stt")]
    public void FilterMatchesMessageSourceOrLevel(string query, string expectedSource)
    {
        var r = Assert.Single(LogParser.Filter(Sample, query));
        Assert.Equal(expectedSource, r.Source);
    }

    [Fact]
    public void FilterNoMatchIsEmpty()
    {
        Assert.Empty(LogParser.Filter(Sample, "zzz"));
    }
}
