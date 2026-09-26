import { describe, it, expect } from "vitest";
import {
  calculateCostFromTokens,
  getPricingForModel,
  pricingKeyForProvider,
} from "../../open-sse/providers/pricing.js";

// rates are $/1M tokens
const per1M = (tokens, rate) => (tokens * rate) / 1_000_000;

describe("calculateCostFromTokens — reasoning tokens", () => {
  // reasoning_tokens is a breakdown WITHIN completion_tokens (see
  // canonicalizeUsage), not an addition to it. Charging both independently
  // billed every reasoning token twice: once at the output rate as part of
  // completion, then again at the reasoning rate. On the gpt-5.6 rates below
  // that was a 48.9% overcharge.
  const gpt56 = { input: 2.5, output: 15, cached: 0.25, reasoning: 15 };
  const tokens = { prompt_tokens: 10_000, completion_tokens: 2_000, cached_tokens: 4_000, reasoning_tokens: 1_500 };

  it("bills reasoning exactly once", () => {
    const expected =
      per1M(10_000 - 4_000, 2.5) +   // non-cached input
      per1M(4_000, 0.25) +            // cache read
      per1M(2_000, 15);               // all output, reasoning rate == output rate
    expect(calculateCostFromTokens(tokens, gpt56)).toBeCloseTo(expected, 10);
  });

  it("does not exceed the plain output rate when reasoning costs the same", () => {
    // 0.0685 was the old (double-charged) figure; 0.046 is correct.
    expect(calculateCostFromTokens(tokens, gpt56)).toBeCloseTo(0.046, 10);
  });

  it("still honours a premium reasoning rate, charging reasoning only at that rate", () => {
    const premium = { input: 1, output: 10, reasoning: 15 };
    const t = { prompt_tokens: 0, completion_tokens: 1_000, reasoning_tokens: 400 };
    // 600 non-reasoning at 10, 400 reasoning at 15.
    expect(calculateCostFromTokens(t, premium)).toBeCloseTo(
      per1M(600, 10) + per1M(400, 15), 10
    );
  });

  it("is unchanged when there are no reasoning tokens", () => {
    const t = { prompt_tokens: 1_000, completion_tokens: 500 };
    expect(calculateCostFromTokens(t, gpt56)).toBeCloseTo(per1M(1_000, 2.5) + per1M(500, 15), 10);
  });

  it("ignores a reasoning count larger than completion rather than going negative", () => {
    const t = { prompt_tokens: 0, completion_tokens: 100, reasoning_tokens: 9_000 };
    expect(calculateCostFromTokens(t, gpt56)).toBeCloseTo(per1M(100, 15), 10);
  });
});

describe("calculateCostFromTokens — free tiers are not re-priced", () => {
  // `pricing.cached || pricing.input` silently re-priced a legitimately free
  // cache tier at the full input rate, because 0 is falsy.
  it("honours an explicit cached rate of 0", () => {
    const cost = calculateCostFromTokens(
      { prompt_tokens: 10_000, cached_tokens: 10_000, completion_tokens: 0 },
      { input: 3, output: 15, cached: 0 }
    );
    expect(cost).toBe(0);
  });

  it("falls back to the input rate when cached is absent, not merely 0", () => {
    const cost = calculateCostFromTokens(
      { prompt_tokens: 10_000, cached_tokens: 10_000, completion_tokens: 0 },
      { input: 3, output: 15 }
    );
    expect(cost).toBeCloseTo(per1M(10_000, 3), 10);
  });

  it("honours an explicit cache_creation rate of 0", () => {
    const cost = calculateCostFromTokens(
      { prompt_tokens: 5_000, cache_creation_input_tokens: 5_000, completion_tokens: 0 },
      { input: 3, output: 15, cache_creation: 0 }
    );
    expect(cost).toBe(0);
  });
});

describe("provider alias resolution", () => {
  // PROVIDER_PRICING is keyed by the registry alias ("gh"), but cost call sites
  // pass the resolved provider id ("github"), so the override was unreachable.
  it("maps the resolved provider id onto the pricing table key", () => {
    expect(pricingKeyForProvider("github")).toBe("gh");
  });

  it("leaves providers with no alias untouched", () => {
    expect(pricingKeyForProvider("openai")).toBe("openai");
    expect(pricingKeyForProvider(undefined)).toBeUndefined();
  });

  it("honours the gh override when given the resolved provider id", () => {
    const viaId = getPricingForModel("github", "gpt-5.3-codex");
    expect(viaId).toBeTruthy();
    expect(viaId).toEqual(getPricingForModel("gh", "gpt-5.3-codex"));
  });
});
