// Request-safety on Cloudflare Workers.
//
// A promise started by a request that is later cancelled (client disconnect, CPU-limit kill) never
// settles on Workers, and neither does a timer that request armed. Module-level shared promises
// therefore hung every later request of the isolate (GET /v1/models requests hung ~330 s).
// These tests simulate the dead owner with a promise that never resolves.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createCountingD1 } from "../helpers/countingD1.js";
import { createFlightGroup, withOwnTimeout, FlightTimeoutError } from "../../open-sse/utils/flight.js";

const NEVER = () => new Promise(() => {});
const tick = (ms) => new Promise((r) => setTimeout(r, ms));

describe("flight group", () => {
  it("coalesces concurrent callers while the leader is alive", async () => {
    const g = createFlightGroup({ followerWaitMs: 500, staleMs: 5000 });
    let calls = 0;
    const work = async () => { calls += 1; await tick(30); return "v"; };
    const [a, b, c] = await Promise.all([g.run("k", work), g.run("k", work), g.run("k", work)]);
    expect(calls).toBe(1);
    expect([a.shared, b.shared, c.shared].sort()).toEqual([false, true, true]);
    expect(a.value).toBe("v");
    expect(g.size()).toBe(0); // nothing is cached after it settles
  });

  it("a follower does its own work when the leader never settles, and is bounded", async () => {
    const g = createFlightGroup({ followerWaitMs: 60, staleMs: 5000 });
    g.run("k", NEVER); // dead owner
    const started = Date.now();
    const { value, shared } = await g.run("k", async () => "own");
    expect(value).toBe("own");
    expect(shared).toBe(false);
    expect(Date.now() - started).toBeLessThan(1000);
  });

  it("an abandoned entry cannot poison later callers (stale entries are replaced)", async () => {
    let t = 1000;
    const g = createFlightGroup({ followerWaitMs: 5000, staleMs: 100, now: () => t });
    g.run("k", NEVER);
    t += 500; // entry is now stale: a new caller must not even wait on it
    const started = Date.now();
    const { value } = await g.run("k", async () => "fresh");
    expect(value).toBe("fresh");
    expect(Date.now() - started).toBeLessThan(500);
  });

  it("a follower retries when the leader rejects", async () => {
    const g = createFlightGroup({ followerWaitMs: 500, staleMs: 5000 });
    let n = 0;
    const work = async () => { n += 1; await tick(10); if (n === 1) throw new Error("boom"); return "ok"; };
    const leader = g.run("k", work);
    const follower = g.run("k", work);
    await expect(leader).rejects.toThrow("boom");
    expect((await follower).value).toBe("ok");
  });

  it("withOwnTimeout rejects with FlightTimeoutError and clears its timer", async () => {
    await expect(withOwnTimeout(NEVER(), 20, "x")).rejects.toBeInstanceOf(FlightTimeoutError);
    await expect(withOwnTimeout(Promise.resolve(5), 20, "x")).resolves.toBe(5);
  });

  it("NINEROUTER_FLIGHT_WAIT_MS overrides the follower wait", async () => {
    process.env.NINEROUTER_FLIGHT_WAIT_MS = "20";
    try {
      const g = createFlightGroup({ followerWaitMs: 60000, staleMs: 60000 });
      g.run("k", NEVER);
      const started = Date.now();
      await g.run("k", async () => 1);
      expect(Date.now() - started).toBeLessThan(1000);
    } finally {
      delete process.env.NINEROUTER_FLIGHT_WAIT_MS;
    }
  });
});

describe("models catalogue", () => {
  it("a build whose owner never settles does not hang the next request", async () => {
    process.env.NINEROUTER_FLIGHT_WAIT_MS = "40";
    try {
      vi.resetModules();
      const { withCatalog } = await import("../../src/lib/modelCatalog/modelsListCache.js");
      const req = (h = {}) => ({ headers: new Headers(h) });
      let resolves = 0;
      const args = (resolve) => ({ request: req(), cacheKey: "k1", resolve, project: async () => "BODY" });
      // owner: its read never completes (cancelled request)
      withCatalog(args(async () => { await NEVER(); }));
      const second = await withCatalog(args(async () => { resolves += 1; return { inputs: {}, fingerprint: "fp1" }; }));
      expect(second.body).toBe("BODY");
      expect(resolves).toBe(1);
    } finally {
      delete process.env.NINEROUTER_FLIGHT_WAIT_MS;
    }
  });
});

// ----- Cloudflare adapter -----

const holder = vi.hoisted(() => ({ d1: null, pending: [], ctxHang: false, syncCtx: null }));
vi.mock("@opennextjs/cloudflare", () => ({
  // A real request returns the context synchronously inside a request; async callers just await it.
  getCloudflareContext: () => {
    if (holder.syncCtx) return holder.syncCtx;
    return (async () => {
      if (holder.ctxHang) { holder.ctxHang = false; return new Promise(() => {}); }
      return { env: { DB: holder.d1 }, ctx: { waitUntil: (p) => holder.pending.push(p) } };
    })();
  },
}));

function seed(d1, rows) {
  d1.raw.exec(`CREATE TABLE IF NOT EXISTS ninerouter_rows (table_name TEXT NOT NULL, row_key TEXT NOT NULL, data TEXT NOT NULL, updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, PRIMARY KEY (table_name, row_key))`);
  const insert = d1.raw.prepare("INSERT INTO ninerouter_rows(table_name, row_key, data) VALUES(?, ?, ?)");
  for (const [table, key, data] of rows) insert.run(table, key, JSON.stringify(data));
}
const kvRow = (scope, key, value) => ["kv", `scope:${scope}|key:${key}`, { scope, key, value: JSON.stringify(value) }];

async function makeAdapter() {
  vi.resetModules();
  const mod = await import("../../src/lib/db/adapters/cloudflareMemoryAdapter.js");
  return mod.createCloudflareMemoryAdapter();
}

describe("cloudflare adapter request safety", () => {
  beforeEach(() => {
    holder.d1 = createCountingD1();
    holder.pending.length = 0;
    holder.ctxHang = false;
    process.env.NINEROUTER_FLIGHT_WAIT_MS = "50";
  });
  afterEach(() => {
    delete process.env.NINEROUTER_FLIGHT_WAIT_MS;
    delete process.env.NINEROUTER_D1_TIMEOUT_MS;
    delete global._dbAdapter;
  });

  it("a second request still loads a kv scope when the first request's load never answers", async () => {
    seed(holder.d1, [kvRow("pricing", "openai", { a: 1 })]);
    const db = await makeAdapter();
    const original = holder.d1.prepare;
    let first = true;
    holder.d1.prepare = (sql) => {
      if (first && sql.includes("row_key >=")) { first = false; return { bind: () => ({ all: NEVER }) }; }
      return original(sql);
    };
    process.env.NINEROUTER_D1_TIMEOUT_MS = "0"; // the dead request has no timer of its own that fires
    db.ensureKvScope("pricing"); // owner: hangs forever
    const started = Date.now();
    await db.ensureKvScope("pricing"); // must not hang
    expect(Date.now() - started).toBeLessThan(2000);
    expect(db.all("SELECT key FROM kv WHERE scope = ?", ["pricing"])).toHaveLength(1);
  });

  it("a concurrent caller never proceeds on a table that is still loading", async () => {
    seed(holder.d1, [["providerConnections", "id:c1", { id: "c1", provider: "openai", authType: "apikey", name: "n", priority: 1, isActive: 1, data: "{}", createdAt: "t", updatedAt: "t" }]]);
    const db = await makeAdapter();
    const original = holder.d1.prepare;
    holder.d1.prepare = (sql) => {
      const st = original(sql);
      if (!sql.includes("table_name IN")) return st;
      return { bind: (...a) => { const b = st.bind(...a); return { all: async () => { await tick(30); return b.all(); } }; } };
    };
    const results = await Promise.all([db.ensureTables(["providerConnections"]).then(() => db.all("SELECT id FROM providerConnections").length), (async () => { await tick(5); await db.ensureTables(["providerConnections"]); return db.all("SELECT id FROM providerConnections").length; })()]);
    expect(results).toEqual([1, 1]);
  });

  it("budget history load is fail-closed: a hung D1 query rejects instead of hanging", async () => {
    seed(holder.d1, []);
    const db = await makeAdapter();
    const original = holder.d1.prepare;
    holder.d1.prepare = (sql) => (sql.includes("usageHistory") && sql.startsWith("SELECT") ? { bind: () => ({ all: NEVER }) } : original(sql));
    process.env.NINEROUTER_D1_TIMEOUT_MS = "60";
    const started = Date.now();
    await expect(db.ensureBudgetHistory({ apiKeyId: "k", apiKey: "sk", since: "2026-01-01T00:00:00Z" })).rejects.toThrow(/timed out/);
    expect(Date.now() - started).toBeLessThan(3000);
    // and a later request is not stuck behind the failure
    holder.d1.prepare = original;
    await db.ensureBudgetHistory({ apiKeyId: "k", apiKey: "sk", since: "2026-01-01T00:00:00Z" });
  });

  it("every D1 call is bounded (guardD1)", async () => {
    const { guardD1 } = await import("../../src/lib/db/adapters/cloudflareMemoryAdapter.js");
    const raw = { prepare: () => ({ bind() { return this; }, all: NEVER, run: NEVER, first: NEVER }), batch: NEVER, exec: NEVER };
    const g = guardD1(raw, () => 30);
    await expect(g.prepare("x").bind(1).all()).rejects.toThrow(/D1 query timed out/);
    await expect(g.prepare("x").run()).rejects.toThrow(/timed out/);
    await expect(g.batch([g.prepare("x")])).rejects.toThrow(/D1 batch timed out/);
    await expect(g.exec("x")).rejects.toThrow(/timed out/);
  });

  it("writes are kept alive on the CURRENT request's waitUntil, not the boot request's", async () => {
    seed(holder.d1, []);
    const db = await makeAdapter();
    const current = [];
    holder.syncCtx = { env: { DB: holder.d1 }, ctx: { waitUntil: (p) => current.push(p) } };
    holder.pending.length = 0;
    db.run("INSERT INTO combos(id, name, models, createdAt, updatedAt) VALUES(?, ?, ?, ?, ?)", ["c1", "n", "[]", "t", "t"]);
    holder.syncCtx = null;
    expect(current.length).toBeGreaterThan(0);
    expect(holder.pending.length).toBe(0);
    await Promise.allSettled(current);
  });
});

describe("getAdapter after an abandoned init", () => {
  beforeEach(() => {
    holder.d1 = createCountingD1();
    seed(holder.d1, []);
    delete global._dbAdapter;
    process.env.NINEROUTER_FLIGHT_WAIT_MS = "50";
  });
  afterEach(() => {
    delete process.env.NINEROUTER_FLIGHT_WAIT_MS;
    delete global._dbAdapter;
  });

  it("the second request initialises its own adapter instead of awaiting the dead one forever", async () => {
    vi.resetModules();
    const { getAdapter } = await import("../../src/lib/db/driver.cloudflare.js");
    holder.ctxHang = true; // the first init never gets its Cloudflare context
    getAdapter(); // owner: dead
    const started = Date.now();
    const adapter = await getAdapter();
    expect(adapter.driver).toMatch(/cloudflare/);
    expect(Date.now() - started).toBeLessThan(3000);
    expect(await getAdapter()).toBe(adapter);
  });
});
