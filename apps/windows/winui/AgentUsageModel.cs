using System.Collections.Generic;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace DontSpeak;

/// <summary>
/// Decoder for shared Usage card contract (Rust <c>UsageDeck</c> / <c>UsageCard</c>).
/// Hosts only paint this model (no agent-specific layout branches).
/// </summary>
internal static class AgentUsageModel
{
    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNameCaseInsensitive = true,
    };

    internal static UsageDeckDto? DecodeDeck(string json)
    {
        if (string.IsNullOrWhiteSpace(json)) return null;
        try
        {
            var deck = JsonSerializer.Deserialize<UsageDeckDto>(json, JsonOptions);
            return deck?.Cards is not null ? deck : null;
        }
        catch (JsonException)
        {
            return null;
        }
    }

    internal static UsageCardDto? DecodeCard(string json)
    {
        if (string.IsNullOrWhiteSpace(json)) return null;
        try
        {
            return JsonSerializer.Deserialize<UsageCardDto>(json, JsonOptions);
        }
        catch (JsonException)
        {
            return null;
        }
    }
}

// Wire DTOs mirror Rust UsageDeck / UsageCard / UsageRow.
internal sealed record UsageDeckDto(
    [property: JsonPropertyName("cards")] List<UsageCardDto> Cards);

internal sealed record UsageCardDto(
    [property: JsonPropertyName("agent")] string Agent,
    [property: JsonPropertyName("rows")] List<UsageRowDto> Rows,
    [property: JsonPropertyName("account")] string? Account = null,
    // Wire key absent when false; true = guarded credentials (authorize unlocks).
    [property: JsonPropertyName("needs_auth")] bool NeedsAuth = false);

internal sealed record UsageRowDto(
    [property: JsonPropertyName("period")] string Period,
    [property: JsonPropertyName("used_percent")] double UsedPercent,
    [property: JsonPropertyName("resets_at_unix")] long ResetsAtUnix);
