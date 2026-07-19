using Xunit;

namespace DontSpeak.Tests;

/// <summary>
/// <see cref="Native.ParseUpdateAvailable"/> / ParseLatestVersion: missing/malformed ⇒ false/null
/// (pill only on clear true). No ds_core.dll or network.
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

    [Theory]
    [InlineData("")]
    [InlineData("   ")]
    [InlineData("{}")]                                   // documented failure sentinel
    [InlineData("not json at all")]
    [InlineData("[1,2,3]")]
    [InlineData("""{"current_version":"0.1.0","latest_version":"0.2.0"}""")]   // missing key
    public void AmbiguousOrMalformedPayloadNeverShowsThePill(string json) =>
        Assert.False(Native.ParseUpdateAvailable(json));

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
    [InlineData("""{"update_available":true}""")]   // available but no latest_version
    public void LatestVersionIsNullOnAnyAmbiguousOrMalformedPayload(string json) =>
        Assert.Null(Native.ParseLatestVersion(json));
}
