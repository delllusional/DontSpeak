using Xunit;

namespace DontSpeak.Tests;

public class AgentUsageModelTests
{
    [Fact]
    public void DecodesDeckAndCard()
    {
        var deck = AgentUsageModel.DecodeDeck(
            """
            {"cards":[
              {"agent":"claude","account":"me@anthropic.test","rows":[
                {"period":"session","used_percent":12,"resets_at_unix":1799990000},
                {"period":"week","used_percent":25.5,"resets_at_unix":1800000000}
              ]},
              {"agent":"grok","rows":[
                {"period":"week","used_percent":8,"resets_at_unix":1801000000}
              ]}
            ]}
            """);

        Assert.NotNull(deck);
        Assert.Equal(2, deck.Cards.Count);
        Assert.Equal("claude", deck.Cards[0].Agent);
        Assert.Equal("me@anthropic.test", deck.Cards[0].Account);
        Assert.Equal("session", deck.Cards[0].Rows[0].Period);
        Assert.Equal(12, deck.Cards[0].Rows[0].UsedPercent);
        Assert.Equal("grok", deck.Cards[1].Agent);
        Assert.Null(deck.Cards[1].Account);

        var card = AgentUsageModel.DecodeCard(
            """{"agent":"codex","account":"dev@openai.com","rows":[{"period":"week","used_percent":63,"resets_at_unix":1800000000}]}""");
        Assert.NotNull(card);
        Assert.Equal("codex", card.Agent);
        Assert.Equal("dev@openai.com", card.Account);
        Assert.Equal(63, card.Rows[0].UsedPercent);
    }

    [Fact]
    public void RejectsMissingCards()
    {
        Assert.Null(AgentUsageModel.DecodeDeck("""{"providers":[]}"""));
    }

    [Fact]
    public void DecodesCanonicalEmptyDeck()
    {
        var deck = AgentUsageModel.DecodeDeck("""{"cards":[]}""");

        Assert.NotNull(deck);
        Assert.Empty(deck.Cards);
    }

    // needs_auth defaults false; present only when true.
    [Fact]
    public void DecodesNeedsAuthDefaultingFalse()
    {
        var plain = AgentUsageModel.DecodeCard("""{"agent":"claude","rows":[]}""");
        Assert.NotNull(plain);
        Assert.False(plain.NeedsAuth);

        var guarded = AgentUsageModel.DecodeCard(
            """{"agent":"claude","rows":[],"needs_auth":true}""");
        Assert.NotNull(guarded);
        Assert.True(guarded.NeedsAuth);
    }

    [Fact]
    public void EmptyAuthorizationResultReplacesAuthPrompt()
    {
        var guarded = new UsageCardDto("claude", [], NeedsAuth: true);
        var unauthorized = new UsageCardDto("claude", []);

        Assert.False(AgentUsageModel.Replaces(guarded, unauthorized));
        Assert.True(AgentUsageModel.Replaces(unauthorized, unauthorized));
        Assert.True(AgentUsageModel.Replaces(
            guarded,
            unauthorized,
            UsageUpdateKind.Authorization));
    }
}
