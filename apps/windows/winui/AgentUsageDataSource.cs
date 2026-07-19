namespace DontSpeak;

/// <summary>Usage C ABI adapter; JSON only at the boundary.</summary>
internal static class AgentUsageDataSource
{
    internal static UsageDeckDto? ReadCachedDeck()
        => AgentUsageModel.DecodeDeck(Native.AgentUsageSkeletonJson());

    internal static UsageCardDto? RefreshCard(string agent)
        => AgentUsageModel.DecodeCard(Native.AgentUsageCardJson(agent, refresh: true));

    /// <summary>Blocking authorize; user click only.</summary>
    internal static UsageCardDto? AuthorizeCard(string agent)
        => AgentUsageModel.DecodeCard(Native.AgentUsageAuthorizeCardJson(agent));
}
