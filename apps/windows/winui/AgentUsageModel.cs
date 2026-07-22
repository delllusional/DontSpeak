using System.Collections.Generic;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace DontSpeak;

internal enum UsageUpdateKind
{
    Refresh,
    Authorization,
}

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

    /// <summary>An explicit authorization result is authoritative, including an empty
    /// result after keychain access succeeds but the credential itself is rejected.</summary>
    internal static bool Replaces(
        UsageCardDto? painted,
        UsageCardDto updated,
        UsageUpdateKind kind = UsageUpdateKind.Refresh)
    {
        if (kind == UsageUpdateKind.Authorization) return true;
        if (updated.Rows.Count > 0 || updated.NeedsAuth) return true;
        return painted is { Rows.Count: 0, NeedsAuth: false }
            && painted.Agent == updated.Agent;
    }
}

// Wire DTOs mirror Rust UsageDeck / UsageCard / UsageRow.
internal sealed record UsageDeckDto(
    [property: JsonPropertyName("cards")] List<UsageCardDto> Cards);

internal sealed record UsageCardDto(
    [property: JsonPropertyName("agent")] string Agent,
    [property: JsonPropertyName("rows")] List<UsageRowDto> Rows,
    [property: JsonPropertyName("account")] string? Account = null,
    // True only for guarded macOS keychain access.
    [property: JsonPropertyName("needs_auth")] bool NeedsAuth = false);

internal sealed record UsageRowDto(
    [property: JsonPropertyName("period")] string Period,
    [property: JsonPropertyName("used_percent")] double UsedPercent,
    [property: JsonPropertyName("resets_at_unix")] long ResetsAtUnix);
