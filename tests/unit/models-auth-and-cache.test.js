import { describe, it, expect, beforeEach, vi } from "vitest";

// The catalogue routes are the most expensive unauthenticated surface in the
// app: each request reads every provider connection and builds capability
// metadata for thousands of models. These tests pin the two properties that
// keep that from being a free amplification point — it requires a real key,
// and a valid caller is answered from cache instead of rebuilt.

const state = {
  keys: new Map(),
  connections: [],
  built: 0,
  lastAllowedModels: undefined,
};

vi.mock("@/lib/localDb", () => ({
  getApiKeyByKey: vi.fn(async (key) => state.keys.get(key) || null),
  getProviderConnections: vi.fn(async () => state.connections),
  getCombos: vi.fn(async () => []),
  getCustomModels: vi.fn(async () => []),
  getModelAliases: vi.fn(async () => []),
  getSettings: vi.fn(async () => ({ modelsListSource: "cached" })),
}));

vi.mock("@/lib/disabledModelsDb", () => ({ getDisabledModels: vi.fn(async () => []) }));

const { GET } = await import("@/app/api/v1/models/route.js");
const { GET: GET_KIND } = await import("@/app/api/v1/models/[...model]/route.js");
const { GET: GET_V1BETA } = await import("@/app/api/v1beta/models/route.js");
const { MODELS_LIST_TTL_MS, MODELS_EDGE_TTL_S, modelsCacheId } = await import(
  "@/lib/modelCatalog/modelsListCache.js"
);

function req(url = "http://localhost/v1/models", headers = {}) {
  return new Request(url, { headers });
}

const FULL_KEY = "sk-full";
const RESTRICTED_KEY = "sk-restricted";

function seed() {
  state.keys = new Map([
    [FULL_KEY, { id: "k-full", isActive: true, allowedModels: "", expiresAt: "" }],
    [RESTRICTED_KEY, { id: "k-restricted", isActive: true, allowedModels: "gpt-4o,auto", expiresAt: "" }],
  ]);
  state.connections = [];
  state.built = 0;
  // The catalogue cache is module-global by design (it is what survives between
  // requests in an isolate). Clear it so each test starts cold and the
  // miss/hit assertions mean what they say.
  globalThis._modelsListCache?.clear?.();
  return `t${Math.random().toString(36).slice(2)}`;
}

describe("models API requires a key", () => {
  let id;
  beforeEach(() => { id = seed(); });

  it("401s without a key on /v1/models", async () => {
    const res = await GET(req());
    expect(res.status).toBe(401);
    expect(await res.json()).toEqual({ error: "API key required" });
  });

  it("401s a key that does not exist", async () => {
    const res = await GET(req("http://localhost/v1/models", { authorization: `Bearer ${id}-nope` }));
    expect(res.status).toBe(401);
  });

  it("401s a disabled key and an expired one", async () => {
    state.keys.set("sk-off", { id: "x", isActive: false, allowedModels: "", expiresAt: "" });
    state.keys.set("sk-old", { id: "y", isActive: true, allowedModels: "", expiresAt: "2000-01-01" });
    expect((await GET(req("http://localhost/v1/models", { authorization: "Bearer sk-off" }))).status).toBe(401);
    expect((await GET(req("http://localhost/v1/models", { authorization: "Bearer sk-old" }))).status).toBe(401);
  });

  it("accepts the key from any header a client actually sends", async () => {
    for (const headers of [
      { authorization: `Bearer ${FULL_KEY}` },
      { "x-api-key": FULL_KEY },
      { "x-goog-api-key": FULL_KEY },
    ]) {
      const res = await GET(req("http://localhost/v1/models", headers));
      expect(res.status).toBe(200);
    }
  });

  it("guards the other two catalogue spellings too", async () => {
    expect((await GET_KIND(req(), { params: Promise.resolve({ model: ["image"] }) })).status).toBe(401);
    expect((await GET_V1BETA(req("http://localhost/v1beta/models"))).status).toBe(401);
  });
});

describe("catalogue caching", () => {
  let id;
  beforeEach(() => { id = seed(); });

  it("serves a repeat call from cache instead of rebuilding", async () => {
    const url = `http://localhost/v1/models?ck=${id}-a`;
    const first = await GET(req(url, { authorization: `Bearer ${FULL_KEY}` }));
    expect(first.headers.get("x-models-cache")).toBe("miss");
    const firstBody = await first.text();
    const second = await GET(req(url, { authorization: `Bearer ${FULL_KEY}` }));
    expect(second.headers.get("x-models-cache")).toBe("memory");
    expect(await second.text()).toBe(firstBody);
  });

  it("caches a restricted key's list separately from the full one", async () => {
    // If these shared a cache entry, one caller would be handed the other's
    // allow-list — the exact leak the per-key cache id prevents.
    const url = `http://localhost/v1/models?ck=${id}-b`;
    const full = await GET(req(url, { authorization: `Bearer ${FULL_KEY}` }));
    const restricted = await GET(req(url, { authorization: `Bearer ${RESTRICTED_KEY}` }));
    expect(restricted.status).toBe(200);
    expect(modelsCacheId(state.keys.get(FULL_KEY))).not.toBe(modelsCacheId(state.keys.get(RESTRICTED_KEY)));

    const fullBody = await full.text();
    const restrictedBody = await restricted.text();
    expect(fullBody).not.toBe(restrictedBody);

    // A second hit for the restricted key must come from its own entry, not
    // from the unrestricted one written first.
    const restrictedAgain = await GET(req(url, { authorization: `Bearer ${RESTRICTED_KEY}` }));
    expect(restrictedAgain.headers.get("x-models-cache")).toBe("memory");
    expect(await restrictedAgain.text()).toBe(restrictedBody);
  });

  it("holds the catalogue for 10 minutes, not less", () => {
    expect(MODELS_LIST_TTL_MS).toBeGreaterThanOrEqual(600_000);
    expect(MODELS_EDGE_TTL_S).toBeGreaterThanOrEqual(600);
  });

  it("marks responses private so a shared cache cannot reuse them", async () => {
    const res = await GET(req(`http://localhost/v1/models?ck=${id}-c`, { authorization: `Bearer ${FULL_KEY}` }));
    expect(res.headers.get("cache-control")).toMatch(/private/);
    expect(res.headers.get("vary")).toMatch(/authorization/i);
  });
});
