import { describe, it, expect, vi, beforeEach } from "vitest";

// The models.dev catalog sync is the fallback pricing tier. A registered model
// with no hand-written rate used to resolve to null, and calculateCost then
// hard-returned 0 — so a paid model billed nothing. This covers that the sync
// emits usable rates and that the resolver consults them last, i.e. only after
// the hand-written tables, so deliberate overrides still win.

const catalog = {
  anthropic: {
    models: {
      "claude-priced": { cost: { input: 3, output: 15, cache_read: 0.3, cache_write: 3.75 } },
      "claude-free": { cost: { input: 0, output: 0 } },
      "claude-nocost": { modalities: { input: ["text"] } },
      "claude-badcost": { cost: { input: "n/a", output: null } },
    },
  },
  openai: {
    models: {
      "gpt-oss-120b": { cost: { input: 0.15, output: 0.6 } },
    },
  },
};

// build() walks the registry snapshot; entries always carry the fields it reads.
const entry = (provider, model) => ({
  provider,
  model,
  contextLength: 200_000,
  current: { contextWindow: 200_000, maxOutput: 8_000 },
});
const entries = [entry("anthropic", "claude-priced"), entry("openai", "gpt-oss-120b")];

describe("model catalog — pricing tier", () => {
  let build;

  beforeEach(async () => {
    vi.resetModules();
    build = (await import("../../src/lib/modelCatalog/sync.js")).build;
  });

  it("emits rates keyed by provider + model, in $/1M tokens", () => {
    const { pricing } = build(catalog, entries);
    expect(pricing["anthropic:claude-priced"]).toEqual({
      input: 3, output: 15, cached: 0.3, cache_creation: 3.75,
    });
  });

  it("records a free model as an explicit zero rather than omitting it", () => {
    const { pricing } = build(catalog, entries);
    expect(pricing["anthropic:claude-free"]).toEqual({ input: 0, output: 0 });
  });

  it("omits cache rates upstream does not publish, so the input rate still applies", () => {
    const { pricing } = build(catalog, entries);
    expect(pricing).not.toHaveProperty("openai:gpt-oss-120b.cached");
    expect(pricing).not.toHaveProperty("openai:gpt-oss-120b.cache_creation");
  });

  it("skips models with no usable cost block", () => {
    const { pricing } = build(catalog, entries);
    expect(pricing).not.toHaveProperty("anthropic:claude-nocost");
    expect(pricing).not.toHaveProperty("anthropic:claude-badcost");
  });

  it("files a model under its upstream id too, for custom provider nodes", () => {
    const { pricing } = build(catalog, entries);
    expect(pricing["anthropic:claude-priced"]).toBeTruthy();
    // A local id that differs from upstream gets its own key, and the upstream
    // key is still reachable.
    const aliased = build(catalog, [entry("claude", "claude-priced"), entry("openai", "gpt-oss-120b")]);
    expect(aliased.pricing["claude:claude-priced"]).toBeTruthy();
    expect(aliased.pricing["anthropic:claude-priced"]).toBeTruthy();
  });
});

describe("getPricingForModel — catalog fallback", () => {
  let getPricingForModel;
  const catalogPricing = {
    "openai:o3": { input: 2, output: 8 },
    "openai:claude-sonnet-4-5": { input: 999, output: 999 }, // must NOT win
  };

  beforeEach(async () => {
    vi.resetModules();
    // pricingRepo reaches the catalog through the source the server injects into
    // capabilities.js (catalogOverride touches node:fs and must not be imported
    // from a repo — that breaks the Cloudflare client build).
    const getPricing = (provider, model) =>
      catalogPricing[`${provider}:${String(model).toLowerCase()}`] || null;
    vi.doMock("open-sse/providers/catalogOverride.js", () => ({ getCatalogPricing: getPricing }));
    const capabilities = await import("../../open-sse/providers/capabilities.js");
    capabilities.setCatalogSource({ getModalities: () => null, getLimits: () => null, getPricing });
    vi.doMock("@/lib/db/driver.js", () => ({
      getAdapter: () => ({ all: () => [] }),
    }));
    getPricingForModel = (await import("../../src/lib/db/repos/pricingRepo.js")).getPricingForModel;
  });

  it("prices a model the hand-written tables miss", async () => {
    // o3 has no MODEL_PRICING entry and PATTERN_PRICING only covers "o3-*".
    const pricing = await getPricingForModel("openai", "o3");
    expect(pricing).toEqual({ input: 2, output: 8 });
  });

  it("leaves the hand-written tables in front of the catalog", async () => {
    const pricing = await getPricingForModel("openai", "claude-sonnet-4-5");
    expect(pricing.input).not.toBe(999);
  });

  it("returns null when neither the tables nor the catalog know the model", async () => {
    expect(await getPricingForModel("openai", "totally-unknown-model")).toBeNull();
  });
});
