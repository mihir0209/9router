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

const state = { keys: new Map(), connections: [], combos: [], buildDelayMs: 0 };

vi.mock("@/lib/localDb", () => ({
  getApiKeyByKey: vi.fn(async (key) => state.keys.get(key) || null),
  getProviderConnections: vi.fn(async () => state.connections),
  getCombos: vi.fn(async () => state.combos),
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
  // The catalogue cache is keyed by key id (it must be: the body depends on the
  // caller's allow-list), so each test gets its own ids or it inherits the
  // previous test's warm entry and cannot observe a first build.
  const suffix = Math.random().toString(36).slice(2);
  state.keys = new Map([
    [FULL_KEY, { id: `k-full-${suffix}`, isActive: true, allowedModels: "", expiresAt: "" }],
    [RESTRICTED_KEY, { id: `k-restricted-${suffix}`, isActive: true, allowedModels: "gpt-4o,auto", expiresAt: "" }],
  ]);
  state.connections = [];
  state.combos = [];
  state.buildDelayMs = 0;
  return suffix;
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

describe("the catalogue is live, and cheap when nothing changed", () => {
  let id;
  beforeEach(() => { id = seed(); });

  it("reuses the body while the inputs are unchanged, and rebuilds when they move", async () => {
    const url = `http://localhost/v1/models?live=${id}-a`;
    const first = await GET(authed(url));
    expect(first.status).toBe(200);
    expect(first.headers.get("x-models-build")).toBe("project");
    const firstBody = await first.text();

    // Nothing changed, so the projection is not repeated. The inputs are still
    // read on every request, which is what keeps this live rather than a TTL.
    const second = await GET(authed(url));
    expect(second.status).toBe(200);
    expect(second.headers.get("x-models-build")).toBe("hash");
    expect(await second.text()).toBe(firstBody);
    expect(second.headers.get("etag")).toBe(first.headers.get("etag"));

    // A connection appears: the very next request must reflect it. A TTL would
    // have hidden this for the remainder of its window.
    state.connections = [{ provider: "openai", isActive: true, authType: "api_key", apiKey: "x" }];
    const third = await GET(authed(url));
    expect(third.headers.get("x-models-build")).toBe("project");
    expect(await third.text()).not.toBe(firstBody);
  });

  it("answers 304 with no body when the caller already holds this ETag", async () => {
    const url = `http://localhost/v1/models?etag=${id}-b`;
    const first = await GET(authed(url));
    const etag = first.headers.get("etag");
    expect(etag).toBeTruthy();
    const body = await first.text();
    expect(body.length).toBeGreaterThan(10);

    const revalidated = await GET(req(url, {
      authorization: `Bearer ${FULL_KEY}`,
      "if-none-match": etag,
    }));
    expect(revalidated.status).toBe(304);
    expect(await revalidated.text()).toBe("");

    // A different ETag gets the body.
    const changed = await GET(req(url, { authorization: `Bearer ${FULL_KEY}`, "if-none-match": '"stale"' }));
    expect(changed.status).toBe(200);
  });

  it("moves the ETag when a combo changes", async () => {
    const url = `http://localhost/v1/models?etagc=${id}-c`;
    const before = (await GET(authed(url))).headers.get("etag");
    state.combos = [{ name: "newcombo", models: ["gpt-4o"] }];
    const after = (await GET(authed(url))).headers.get("etag");
    expect(after).not.toBe(before);
  });

  it("tells the client the body is per-caller and must be revalidated", async () => {
    const res = await GET(authed(`http://localhost/v1/models?hdr=${id}-c`));
    const cc = res.headers.get("cache-control");
    expect(cc).toMatch(/private/);
    expect(res.headers.get("vary")).toMatch(/authorization/i);
  });

  it("never serves one key's allow-list to another", async () => {
    const url = `http://localhost/v1/models?sep=${id}-d`;
    const full = await (await GET(authed(url, FULL_KEY))).json();
    const restricted = await (await GET(authed(url, RESTRICTED_KEY))).json();
    const restrictedAgain = await (await GET(authed(url, RESTRICTED_KEY))).json();
    expect(JSON.stringify(restricted)).toBe(JSON.stringify(restrictedAgain));
    expect(JSON.stringify(restricted)).not.toBe(JSON.stringify(full));
  });
});

describe("concurrent builds are collapsed", () => {
  let id;
  beforeEach(() => { id = seed(); });

  it("does one read and one projection for a burst of identical callers", async () => {
    const { getProviderConnections } = await import("@/lib/localDb");
    let reads = 0;
    getProviderConnections.mockImplementation(async () => {
      reads += 1;
      await new Promise((r) => setTimeout(r, 25));
      return state.connections;
    });

    const url = `http://localhost/v1/models?burst=${id}`;
    const responses = await Promise.all([GET(authed(url)), GET(authed(url)), GET(authed(url))]);
    for (const res of responses) {
      expect(res.status).toBe(200);
      expect(res.headers.get("x-models-build")).toBe("project");
    }
    // Three concurrent callers, one read and one projection. Coalescing only the
    // projection would still leave each caller re-reading the database.
    expect(reads).toBe(1);

    // The next request re-reads to check the fingerprint, then reuses the body.
    const after = await GET(authed(url));
    expect(reads).toBe(2);
    expect(after.headers.get("x-models-build")).toBe("hash");
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
