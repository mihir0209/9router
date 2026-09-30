import { describe, it, expect, beforeEach, vi } from "vitest";

// The catalogue routes are the most expensive unauthenticated surface in the
// app: each request reads every provider connection and builds capability
// metadata for thousands of models. Two properties matter here and they pull in
// opposite directions — it must require a real key, and it must answer from
// current state rather than a cached response.
//
// There is deliberately no TTL. An earlier version cached for 10 minutes, which
// meant a caller relying on this endpoint as its source of truth could not tell
// it was being served a stale list. Cost is handled by rejecting the
// unauthenticated case before any of this runs, by memoizing capability
// derivation (a pure function, so its memo cannot go stale), and by collapsing
// concurrent builds.

const state = { keys: new Map(), connections: [], buildDelayMs: 0 };

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

const FULL_KEY = "sk-full";
const RESTRICTED_KEY = "sk-restricted";

function req(url = "http://localhost/v1/models", headers = {}) {
  return new Request(url, { headers });
}

const authed = (url, key = FULL_KEY) => req(url, { authorization: `Bearer ${key}` });

function seed() {
  state.keys = new Map([
    [FULL_KEY, { id: "k-full", isActive: true, allowedModels: "", expiresAt: "" }],
    [RESTRICTED_KEY, { id: "k-restricted", isActive: true, allowedModels: "gpt-4o,auto", expiresAt: "" }],
  ]);
  state.connections = [];
  state.buildDelayMs = 0;
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
      expect((await GET(req(`http://localhost/v1/models?h=${id}-${Math.random()}`, headers))).status).toBe(200);
    }
  });

  it("guards the other two catalogue spellings too", async () => {
    expect((await GET_KIND(req(), { params: Promise.resolve({ model: ["image"] }) })).status).toBe(401);
    expect((await GET_V1BETA(req("http://localhost/v1beta/models"))).status).toBe(401);
  });
});

describe("the catalogue is always live", () => {
  let id;
  beforeEach(() => { id = seed(); });

  it("rebuilds on every call rather than serving a cached body", async () => {
    const url = `http://localhost/v1/models?live=${id}-a`;
    const first = await GET(authed(url));
    const second = await GET(authed(url));
    expect(first.status).toBe(200);
    expect(second.status).toBe(200);
    // No x-models-cache header any more: there is nothing to report, because
    // nothing is being reused.
    expect(second.headers.get("x-models-cache")).toBeNull();
  });

  it("reflects a config change on the very next request", async () => {
    const url = `http://localhost/v1/models?live=${id}-b`;
    const before = await (await GET(authed(url))).json();
    expect(Array.isArray(before.data)).toBe(true);

    // A connection appears. With a TTL this would not be visible for minutes.
    state.connections = [{ provider: "openai", isActive: true, authType: "api_key", apiKey: "x" }];
    const after = await (await GET(authed(url))).json();
    expect(JSON.stringify(after)).not.toBe(JSON.stringify(before));
  });

  it("tells the client the body is per-caller and must be revalidated", async () => {
    const res = await GET(authed(`http://localhost/v1/models?live=${id}-c`));
    const cc = res.headers.get("cache-control");
    // private: a restricted key's allow-list must not be stored by a shared cache.
    expect(cc).toMatch(/private/);
    // no-cache: storable by the client, but revalidated before reuse, so the
    // endpoint stays authoritative and the caching decision is the caller's.
    expect(cc).toMatch(/no-cache/);
    expect(res.headers.get("vary")).toMatch(/authorization/i);
  });

  it("still gives a restricted key only its allow-list, on every call", async () => {
    const url = `http://localhost/v1/models?live=${id}-d`;
    const first = await (await GET(authed(url, RESTRICTED_KEY))).json();
    const second = await (await GET(authed(url, RESTRICTED_KEY))).json();
    const full = await (await GET(authed(url, FULL_KEY))).json();
    expect(JSON.stringify(first)).toBe(JSON.stringify(second));
    expect(JSON.stringify(first)).not.toBe(JSON.stringify(full));
  });
});

describe("concurrent builds are collapsed", () => {
  let id;
  beforeEach(() => { id = seed(); });

  it("coalesces simultaneous requests into one build, without ever going stale", async () => {
    const { getProviderConnections } = await import("@/lib/localDb");
    let calls = 0;
    getProviderConnections.mockImplementation(async () => {
      calls += 1;
      await new Promise((r) => setTimeout(r, 25));
      return state.connections;
    });

    const url = `http://localhost/v1/models?burst=${id}`;
    const responses = await Promise.all([GET(authed(url)), GET(authed(url)), GET(authed(url))]);
    for (const res of responses) expect(res.status).toBe(200);
    // Three concurrent callers, one build. Afterwards the count rises again,
    // because nothing is retained: the next request is a fresh build.
    expect(calls).toBe(1);
    // With no response cache, this header is the only way to tell a shared build
    // from a fresh one, so it has to be right.
    const reports = responses.map((r) => r.headers.get("x-models-build"));
    expect(reports.filter((v) => v === "fresh")).toHaveLength(1);
    expect(reports.filter((v) => v === "coalesced")).toHaveLength(2);

    const after = await GET(authed(url));
    expect(calls).toBe(2);
    expect(after.headers.get("x-models-build")).toBe("fresh");
  });
});

describe("allow-listed keys honour the requested capability", () => {
  let id;
  beforeEach(() => { id = seed(); });

  // Regression: the restricted branch built the allow-list expansion and
  // returned without consulting kindFilter, so /v1/models/image answered a
  // restricted key with its chat models. Every capability route returned the
  // same list, which made the kind URLs useless for exactly the keys that need
  // them most.
  it("does not answer /v1/models/{kind} with the LLM list", async () => {
    const url = `http://localhost/v1/models/image?k=${id}`;
    const res = await GET_KIND(authed(url, RESTRICTED_KEY), {
      params: Promise.resolve({ model: ["image"] }),
    });
    expect(res.status).toBe(200);
    const body = await res.json();
    // The key is allowed gpt-4o and auto, which are chat models, so an image
    // listing is legitimately empty. What matters is that it is not the chat list.
    expect(body.data.map((m) => m.id)).not.toContain("gpt-4o");
  });
});
