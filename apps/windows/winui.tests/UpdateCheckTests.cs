using Xunit;

namespace DontSpeak.Tests;

/// <summary>
/// The pure parse half of the startup update check (<see cref="Native.ParseUpdateAvailable"/>) —
/// exercises the "missing/malformed ⇒ false, never show the pill on ambiguity" contract from
/// ds_update_check_json's JSON shape, without needing ds_core.dll or a network call.
/// </summary>
public class UpdateCheckTests
{
    [Fact]
    public void UpdateAvailableTrueShowsThePill()
    {
        Assert.True(Native.ParseUpdateAvailable(
            """{"update_available":true,"current_version":"0.1.0","latest_version":"0.2.0","html_url":"https://github.com/delllusional/DontSpeak/releases/tag/v0.2.0"}"""));
    }

    [Fact]
    public void UpdateAvailableFalseHidesThePill()
    {
        Assert.False(Native.ParseUpdateAvailable(
            """{"update_available":false,"current_version":"0.1.0","latest_version":"0.1.0"}"""));
    }

    // ── Every ambiguous/failure shape must resolve to false — never "show the pill anyway" ──

    [Theory]
    [InlineData("")]
    [InlineData("   ")]
    [InlineData("{}")]                                   // the documented failure sentinel
    [InlineData("not json at all")]
    [InlineData("[1,2,3]")]
    [InlineData("""{"current_version":"0.1.0","latest_version":"0.2.0"}""")]   // missing the key
    public void AmbiguousOrMalformedPayloadNeverShowsThePill(string json) =>
        Assert.False(Native.ParseUpdateAvailable(json));

    // ── Native.ParseLatestVersion: null whenever ParseUpdateAvailable would be false ──

    [Fact]
    public void LatestVersionIsReturnedWhenAnUpdateIsAvailable()
    {
        Assert.Equal("0.2.0", Native.ParseLatestVersion(
            """{"update_available":true,"current_version":"0.1.0","latest_version":"0.2.0"}"""));
    }

    [Fact]
    public void LatestVersionIsNullWhenNoUpdateIsAvailable() =>
        Assert.Null(Native.ParseLatestVersion(
            """{"update_available":false,"current_version":"0.1.0","latest_version":"0.1.0"}"""));

    [Theory]
    [InlineData("{}")]
    [InlineData("not json at all")]
    [InlineData("""{"update_available":true}""")]   // available, but no latest_version at all
    public void LatestVersionIsNullOnAnyAmbiguousOrMalformedPayload(string json) =>
        Assert.Null(Native.ParseLatestVersion(json));
}
