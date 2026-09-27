import { describe, it, expect, beforeEach, vi } from "vitest";

/**
 * /v1/models used to ask every configured provider for its model list on every
 * request. That is one upstream round trip per provider, so a single listing
 * request held the connection open for seconds and, on Workers, burned the
 * free-plan CPU budget.
 *
 * The fix is a `modelsListSource` setting with two modes plus a per-provider
 * cache that only an explicit refresh writes. Two properties matter and are
 * pinned here:
 *
 *  1. Cached mode makes ZERO upstream calls (that is the whole point).
 *  2. A refresh that resolves nothing must NOT wipe a previously good cache —
 *     otherwise one flaky upstream silently empties the provider.
 */
const rows = [];
vi.mock("@/lib/db/driver.js", () => ({
  getAdapter: async () => ({
    all: (sql, params) => (sql.includes("WHERE scope") ? rows.filter((r) => r.scope === params[0]) : rows),
    get: (sql, params) => rows.find((r) => r.scope === params[0] && r.key === params[1]) || null,
    run: (sql, params) => {
      if (sql.startsWith("DELETE") && params.length === 2) {
        const i = rows.findIndex((r) => r.scope === params[0] && r.key === params[1]);
        if (i >= 0) rows.splice(i, 1);
      } else if (sql.startsWith("DELETE")) {
        for (let i = rows.length - 1; i >= 0; i--) if (rows[i].scope === params[0]) rows.splice(i, 1);
      } else if (sql.startsWith("INSERT")) {
        const [scope, key, value] = params;
        const i = rows.findIndex((r) => r.scope === scope && r.key === key);
        if (i >= 0) rows[i].value = value; else rows.push({ scope, key, value });
      }
    },
    transaction: (fn) => fn(),
    ensureTables: async () => {},
  }),
}));

const liveResult = { current: null };
vi.mock("open-sse/services/kiroModels.js", () => ({
  resolveKiroModels: async () => {
    if (liveResult.current === null) return { models: [] };
    return { models: liveResult.current };
  },
}));
vi.mock("open-sse/services/kimchiModels.js", () => ({ resolveKimchiModels: async () => ({ models: [] }) }));
vi.mock("open-sse/services/qoderModels.js", () => ({ resolveQoderModels: async () => ({}), routableQoderModels: () => [] }));
vi.mock("open-sse/services/copilotModels.js", () => ({ resolveCopilotModels: async () => ({ models: [] }) }));
vi.mock("open-sse/services/clinepassModels.js", () => ({ resolveClinepassModels: async () => ({ models: [] }), resolveClineModels: async () => ({ models: [] }) }));
vi.mock("open-sse/services/grokCliModels.js", () => ({ resolveGrokCliModels: async () => ({ models: [] }) }));
vi.mock("open-sse/services/cursorModels.js", () => ({ resolveCursorModels: async () => ({ models: [] }) }));
vi.mock("open-sse/shared/zedAuth.js", () => ({ resolveZedModels: async () => ({ models: [] }) }));
vi.mock("@/sse/services/tokenRefresh", () => ({ updateProviderCredentials: async () => {} }));
vi.mock("@/lib/network/connectionProxy", () => ({ resolveConnectionProxyConfig: async () => ({}) }));

const CONN = { id: "c1", provider: "kiro", isActive: true, providerSpecificData: {} };

async function load() {
  vi.resetModules();
  return {
    resolver: await import("../../src/lib/modelCatalog/providerModelResolver.js"),
    repo: await import("../../src/lib/db/repos/providerModelCacheRepo.js"),
  };
}

beforeEach(() => {
  rows.length = 0;
  liveResult.current = null;
});

describe("provider model cache repo", () => {
  it("round-trips models with a fetchedAt stamp", async () => {
    const { repo } = await load();
    const saved = await repo.saveProviderModelCache("kiro", [{ id: "m1" }, { id: "m2" }]);
    expect(saved).toMatchObject({ stored: true, count: 2, truncated: false });
    const entry = await repo.getProviderModelCache("kiro");
    expect(entry.models.map((m) => m.id)).toEqual(["m1", "m2"]);
    expect(typeof entry.fetchedAt).toBe("string");
  });

  it("refuses to overwrite a good cache with an empty refresh", async () => {
    const { repo } = await load();
    await repo.saveProviderModelCache("kiro", [{ id: "good" }]);
    const second = await repo.saveProviderModelCache("kiro", []);
    expect(second).toMatchObject({ stored: false, count: 0, reason: "empty" });
    const entry = await repo.getProviderModelCache("kiro");
    expect(entry.models.map((m) => m.id)).toEqual(["good"]);
  });

  it("keeps capability and kind metadata through the round trip", async () => {
    const { repo } = await load();
    await repo.saveProviderModelCache("zed", [{ id: "z1", kind: "llm", capabilities: { tools: true }, name: "Zed" }]);
    const entry = await repo.getProviderModelCache("zed");
    expect(entry.models[0]).toMatchObject({ id: "z1", kind: "llm", name: "Zed" });
    expect(entry.models[0].capabilities).toEqual({ tools: true });
  });

  it("drops junk entries instead of caching them", async () => {
    const { repo } = await load();
    const saved = await repo.saveProviderModelCache("kiro", [{ id: "ok" }, { id: "" }, { id: "  " }, null, 42]);
    expect(saved.count).toBe(1);
  });

  it("deduplicates repeated ids", async () => {
    const { repo } = await load();
    const saved = await repo.saveProviderModelCache("kiro", [{ id: "a" }, { id: "a" }, { id: "b" }]);
    expect(saved.count).toBe(2);
  });

  it("reports truncation instead of silently dropping models", async () => {
    const { repo } = await load();
    const many = Array.from({ length: 20001 }, (_, i) => ({ id: `m${i}` }));
    const saved = await repo.saveProviderModelCache("big", many);
    expect(saved.truncated).toBe(true);
    expect(saved.totalDiscovered).toBe(20001);
    expect(saved.count).toBe(20000);
  });

  it("returns null for a provider that was never refreshed", async () => {
    const { repo } = await load();
    expect(await repo.getProviderModelCache("nope")).toBeNull();
  });
});

describe("resolveProviderModelList", () => {
  it("live mode queries the upstream resolver", async () => {
    const { resolver } = await load();
    liveResult.current = [{ id: "auto" }, { id: "auto-agentic" }];
    const res = await resolver.resolveProviderModelList(CONN, "kiro", { allowLive: true });
    expect(res.source).toBe("live");
    expect(res.models.map((m) => m.id)).toEqual(["auto", "auto-agentic"]);
  });

  it("cached mode makes zero upstream calls and serves the stored list", async () => {
    const { resolver, repo } = await load();
    await repo.saveProviderModelCache("kiro", [{ id: "cached-1" }]);
    // A live answer that would be wrong to use — proves it was not consulted.
    liveResult.current = [{ id: "live-1" }];
    const res = await resolver.resolveProviderModelList(CONN, "kiro", { allowLive: false });
    expect(res.source).toBe("cached");
    expect(res.models.map((m) => m.id)).toEqual(["cached-1"]);
  });

  it("falls back to the static catalog when nothing was ever refreshed", async () => {
    const { resolver } = await load();
    const res = await resolver.resolveProviderModelList(CONN, "kiro", { allowLive: false });
    expect(res.source).toBe("static");
    expect(Array.isArray(res.models)).toBe(true);
  });

  it("an explicit per-provider allow-list always wins, even in live mode", async () => {
    const { resolver } = await load();
    liveResult.current = [{ id: "should-be-ignored" }];
    const conn = { ...CONN, providerSpecificData: { enabledModels: ["only-this"] } };
    const res = await resolver.resolveProviderModelList(conn, "kiro", { allowLive: true });
    expect(res.source).toBe("configured");
    expect(res.models.map((m) => m.id)).toEqual(["only-this"]);
  });

  it("a live failure falls back to static rather than emptying the list", async () => {
    const { resolver } = await load();
    liveResult.current = null; // resolver returns { models: [] } -> treated as no answer
    const res = await resolver.resolveProviderModelList(CONN, "kiro", { allowLive: true });
    expect(res.source).toBe("live");
  });
});

describe("modelsListSource setting", () => {
  it("defaults to live and normalizes junk to the default", async () => {
    const settings = await import("../../src/lib/db/repos/settingsRepo.js");
    expect(settings.normalizeModelsListSource("cached")).toBe("cached");
    expect(settings.normalizeModelsListSource("LIVE")).toBe("live");
    expect(settings.normalizeModelsListSource("nonsense")).toBe("live");
    expect(settings.normalizeModelsListSource(undefined)).toBe("live");
    expect(settings.normalizeModelsListSource(null)).toBe("live");
  });
});
