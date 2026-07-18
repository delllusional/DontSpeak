namespace DontSpeak;

/// <summary>
/// Lowest-level typed adapter for the Usage C ABI. JSON exists only across that boundary;
/// callers receive domain DTOs and never decode transport payloads themselves.
/// </summary>
internal static class AgentUsageDataSource
{
    internal static UsageDeckDto? ReadCachedDeck()
        => AgentUsageModel.DecodeDeck(Native.AgentUsageSkeletonJson());

    internal static UsageCardDto? RefreshCard(string agent)
        => AgentUsageModel.DecodeCard(Native.AgentUsageCardJson(agent, forceRefresh: true));
}
