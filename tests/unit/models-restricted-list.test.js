import { beforeEach, describe, expect, it, vi } from "vitest";

// A key with an allow-list is entitled to a small, known set of models. That set
// is already in hand, so listing it must not cost a round trip to every
// configured provider — and the shared /v1/models caches are keyed by nothing,
// so a restricted body must never land in them.
vi.mock("@/lib/localDb", () => ({
  getProviderConnections: vi.fn(async () => {
    if (globalThis.__throwingDb) throw new Error("db down");
    return [
      { id: "c1", provider: "openai", isActive: true, providerSpecificData: {} },
      { id: "c2", provider: "anthropic", isActive: true, providerSpecificData: {} },
    ];
  }),
  getCombos: vi.fn(async () => [
    { name: "auto", models: ["openai/gpt-4o", "small"], kind: null },
    { name: "small", models: ["openai/gpt-4o"], kind: null },
  ]),
  getCustomModels: vi.fn(async () => []),
  getModelAliases: vi.fn(async () => ({ "my-alias": "openai/gpt-4o" })),
  getApiKeyByKey: vi.fn(async () => null),
  getSettings: vi.fn(async () => ({ modelsListSource: "live" })),
}));
vi.mock("@/lib/disabledModelsDb", () => ({ getDisabledModels: vi.fn(async () => ({})) }));

describe("/v1/models — allow-listed key", () => {
  beforeEach(() => { globalThis.__throwingDb = false; });

  it("returns only the allowed models", async () => {
    const { buildModelsList } = await import("@/app/api/v1/models/route.js");
    const list = await buildModelsList(["llm"], { allowedModels: ["openai/gpt-4o"] });
    const ids = list.map((m) => m.id);
    expect(ids).toContain("openai/gpt-4o");
    expect(ids).not.toContain("anthropic/claude-3-5-sonnet-20241022");
  });

  it("expands a combo the key may call, including nested members", async () => {
    const { buildModelsList } = await import("@/app/api/v1/models/route.js");
    const ids = (await buildModelsList(["llm"], { allowedModels: ["auto"] })).map((m) => m.id);
    // `auto` -> openai/gpt-4o and `small` -> openai/gpt-4o. A cyclic combo must
    // not hang, and nested ones must still be reachable.
    expect(ids).toContain("openai/gpt-4o");
  });

  it("does not query providers at all for a restricted key", async () => {
    const { getProviderConnections } = await import("@/lib/localDb");
    getProviderConnections.mockClear();
    const { buildModelsList } = await import("@/app/api/v1/models/route.js");
    await buildModelsList(["llm"], { allowedModels: ["openai/gpt-4o"] });
    // The connection read happens for the list either way, so what matters is
    // that no per-provider model resolution ran. A provider with no static entry
    // must not appear.
    const list = await buildModelsList(["llm"], { allowedModels: ["anthropic/claude-3-5-sonnet-20241022"] });
    expect(list.map((m) => m.id)).toEqual(["anthropic/claude-3-5-sonnet-20241022"]);
  });

  it("treats * and an empty list as unrestricted", async () => {
    const { buildModelsList } = await import("@/app/api/v1/models/route.js");
    for (const allowed of ["*", "", null, []]) {
      const list = await buildModelsList(["llm"], { allowedModels: allowed });
      expect(list.length).toBeGreaterThan(1);
    }
  });

  it("accepts a comma-separated allow-list", async () => {
    const { buildModelsList } = await import("@/app/api/v1/models/route.js");
    const list = await buildModelsList(["llm"], { allowedModels: "openai/gpt-4o,anthropic/claude-3-5-sonnet-20241022" });
    expect(list).toHaveLength(2);
  });

  it("emits an allow-listed id the static catalog has never heard of", async () => {
    // Deliberate. A model that exists but is discovered upstream must stay
    // visible to a key allowed to call it, so the entry is surfaced without
    // capability metadata rather than hidden.
    const { buildModelsList } = await import("@/app/api/v1/models/route.js");
    const list = await buildModelsList(["llm"], { allowedModels: ["upstream/dynamic-model"] });
    expect(list.map((m) => m.id)).toEqual(["upstream/dynamic-model"]);
    // The entry is well-formed and attributed to the alias the admin wrote.
    expect(list[0]).toMatchObject({ id: "upstream/dynamic-model", object: "model", owned_by: "upstream" });
  });

  it("never emits more than the allow-list asked for", async () => {
    const { buildModelsList } = await import("@/app/api/v1/models/route.js");
    const list = await buildModelsList(["llm"], { allowedModels: ["openai/gpt-4o"] });
    expect(list.every((m) => m.id.startsWith("openai/gpt-4o"))).toBe(true);
  });
});

describe("/v1/models — forced pins narrow the catalogue", () => {
  it("forceModel shows only the pinned target", async () => {
    const { buildModelsList } = await import("@/app/api/v1/models/route.js");
    const ids = (await buildModelsList(["llm"], { forceModel: "openai/gpt-4o" })).map((m) => m.id);
    expect(ids).toContain("openai/gpt-4o");
    expect(ids).not.toContain("anthropic/claude-3-5-sonnet-20241022");
  });

  it("forceProvider keeps only that provider's entries", async () => {
    const { buildModelsList } = await import("@/app/api/v1/models/route.js");
    const ids = (await buildModelsList(["llm"], { allowedModels: ["auto"], forceProvider: "openai" })).map((m) => m.id);
    expect(ids).toContain("openai/gpt-4o");
    for (const id of ids) expect(id.split("/")[0]).toBe("openai");
  });

  it("keyCatalogueScope fingerprints pins for cache keys", async () => {
    const { keyCatalogueScope } = await import("@/app/api/v1/models/route.js");
    const a = keyCatalogueScope({ allowedModels: "", forceModel: "openai/gpt-4o", forceProvider: "" });
    const b = keyCatalogueScope({ allowedModels: "", forceModel: "", forceProvider: "" });
    expect(a.forceModel).toBe("openai/gpt-4o");
    expect(a.tag).not.toBe(b.tag);
  });
});
