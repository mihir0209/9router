// Cloudflare adapter: per-scope kv hydration, point-loaded usageDaily rows,
// DDL only when the table is missing, and read-freshness tracking.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createCountingD1 } from "../helpers/countingD1.js";

const holder = vi.hoisted(() => ({ d1: null, pending: [] }));
vi.mock("@opennextjs/cloudflare", () => ({
  getCloudflareContext: async () => ({ env: { DB: holder.d1 }, ctx: { waitUntil: (p) => holder.pending.push(p) } }),
}));

const settle = async () => { while (holder.pending.length) await Promise.allSettled(holder.pending.splice(0)); };

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

beforeEach(() => {
  holder.d1 = createCountingD1();
  holder.pending.length = 0;
});

describe("kv scope hydration", () => {
  beforeEach(() => {
    const rows = [kvRow("modelAliases", "fast", "openai/gpt-4o-mini"), kvRow("pricing", "openai", { a: 1 }), kvRow("disabledModels", "x", ["m"])];
    for (let i = 0; i < 500; i++) rows.push(kvRow("customModels", `p|m${i}|llm`, { id: `m${i}` }));
    seed(holder.d1, rows);
  });

  it("loads only the requested scope, with a range read that scans only that scope", async () => {
    const db = await makeAdapter();
    holder.d1.reset();
    await db.ensureKvScope("modelAliases");
    expect(holder.d1.snapshot()).toMatchObject({ queries: 1, rowsRead: 1 });
    expect(db.all("SELECT key, value FROM kv WHERE scope = ?", ["modelAliases"])).toHaveLength(1);
    expect(db.all("SELECT key, value FROM kv WHERE scope = ?", ["customModels"])).toHaveLength(0);
  });

  it("loads a scope once, even for concurrent callers", async () => {
    const db = await makeAdapter();
    holder.d1.reset();
    await Promise.all([db.ensureKvScope("pricing"), db.ensureKvScope("pricing"), db.ensureKvScope("pricing")]);
    await db.ensureKvScope("pricing");
    expect(holder.d1.snapshot().queries).toBe(1);
  });

  it("does not confuse scopes that share a name prefix", async () => {
    seed(holder.d1, [kvRow("pricing2", "z", 1)]);
    const db = await makeAdapter();
    await db.ensureKvScope("pricing");
    expect(db.all("SELECT key FROM kv WHERE scope = ?", ["pricing2"])).toHaveLength(0);
    expect(db.all("SELECT key FROM kv WHERE scope = ?", ["pricing"])).toHaveLength(1);
  });

  it("ensureTables(['kv']) completes the table without duplicating resident rows", async () => {
    const db = await makeAdapter();
    await db.ensureKvScope("modelAliases");
    await db.ensureTables(["kv"]);
    expect(db.all("SELECT key FROM kv WHERE scope = ?", ["modelAliases"])).toHaveLength(1);
    expect(db.all("SELECT key FROM kv WHERE scope = ?", ["customModels"])).toHaveLength(500);
    holder.d1.reset();
    await db.ensureKvScope("pricing"); // table is whole now: no further reads
    expect(holder.d1.snapshot().queries).toBe(0);
  });

  it("a local write to an unloaded scope is persisted row-level and survives the later load", async () => {
    const db = await makeAdapter();
    holder.d1.reset();
    db.run("INSERT INTO kv(scope, key, value) VALUES(?, ?, ?)", ["pricing", "anthropic", JSON.stringify({ b: 2 })]);
    await settle();
    expect(holder.d1.snapshot()).toMatchObject({ queries: 1, rowsWritten: 1 });
    await db.ensureKvScope("pricing");
    expect(db.all("SELECT key FROM kv WHERE scope = ?", ["pricing"]).map((r) => r.key).sort()).toEqual(["anthropic", "openai"]);
  });

  it("a failed scope load can be retried", async () => {
    const db = await makeAdapter();
    const original = holder.d1.prepare;
    let failures = 1;
    holder.d1.prepare = (sql) => {
      if (failures > 0 && sql.includes("row_key >=")) { failures -= 1; return { bind: () => ({ all: async () => { throw new Error("boom"); } }) }; }
      return original(sql);
    };
    await db.ensureKvScope("pricing");
    expect(db.all("SELECT key FROM kv WHERE scope = ?", ["pricing"])).toHaveLength(0);
    await db.ensureKvScope("pricing");
    expect(db.all("SELECT key FROM kv WHERE scope = ?", ["pricing"])).toHaveLength(1);
  });
});

describe("usageDaily is point-loaded", () => {
  it("is not read at init, and ensureRow pulls exactly one day", async () => {
    const rows = [];
    for (let d = 1; d <= 30; d++) {
      const dateKey = `2026-09-${String(d).padStart(2, "0")}`;
      rows.push(["usageDaily", `dateKey:${dateKey}`, { dateKey, data: JSON.stringify({ requests: d }) }]);
    }
    seed(holder.d1, rows);
    holder.d1.reset();
    const db = await makeAdapter();
    expect(holder.d1.snapshot().rowsRead).toBe(0); // 30 history rows stay in D1
    holder.d1.reset();
    await db.ensureRow("usageDaily", "dateKey:2026-09-07");
    await db.ensureRow("usageDaily", "dateKey:2026-09-07");
    expect(holder.d1.snapshot()).toMatchObject({ queries: 1, rowsRead: 1 });
    expect(db.get("SELECT data FROM usageDaily WHERE dateKey = ?", ["2026-09-07"])).toBeTruthy();
    expect(db.get("SELECT data FROM usageDaily WHERE dateKey = ?", ["2026-09-08"])).toBeFalsy();
  });
});

describe("init and freshness", () => {
  it("does not run the CREATE TABLE statement when the table exists", async () => {
    seed(holder.d1, []);
    holder.d1.reset();
    await makeAdapter();
    expect(holder.d1.snapshot().byKind.ddl).toBeUndefined();
  });

  it("creates the table on a brand-new database and keeps working", async () => {
    const db = await makeAdapter();
    expect(holder.d1.snapshot().byKind.ddl).toBe(1);
    db.run("INSERT INTO combos(id, name, models, createdAt, updatedAt) VALUES(?, ?, ?, ?, ?)", ["c1", "n", "[]", "t", "t"]);
    await settle();
    expect(holder.d1.rowCount("combos")).toBe(1);
  });

  it("freshWithin reflects the last read of a table", async () => {
    seed(holder.d1, []);
    const db = await makeAdapter();
    expect(db.freshWithin("apiKeys", 1000)).toBe(true);
    expect(db.freshWithin("providerConnections", 1000)).toBe(false); // lazy, never read
    vi.useFakeTimers({ toFake: ["Date"], now: Date.now() + 5000 });
    try {
      expect(db.freshWithin("apiKeys", 1000)).toBe(false);
      await db.resync(true, ["apiKeys"]);
      expect(db.freshWithin("apiKeys", 1000)).toBe(true);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("repos that read kv directly see persisted rows on a cold isolate", () => {
  const original = process.env.NINEROUTER_RUNTIME;
  beforeEach(() => {
    process.env.NINEROUTER_RUNTIME = "cloudflare";
    delete global._dbAdapter;
    seed(holder.d1, [
      kvRow("disabledModels", "openai", ["gpt-3"]),
      kvRow("pricing", "openai", { "gpt-x": { input: 1, output: 2 } }),
      kvRow("customModels", "openai|m1|llm", { providerAlias: "openai", id: "m1", type: "llm", name: "M1" }),
    ]);
  });
  afterEach(() => {
    if (original === undefined) delete process.env.NINEROUTER_RUNTIME; else process.env.NINEROUTER_RUNTIME = original;
    delete global._dbAdapter;
  });

  it("getDisabledModels / getDisabledByProvider", async () => {
    vi.resetModules();
    const repo = await import("@/lib/db/repos/disabledModelsRepo.js");
    expect(await repo.getDisabledModels()).toEqual({ openai: ["gpt-3"] });
    expect(await repo.getDisabledByProvider("openai")).toEqual(["gpt-3"]);
  });

  it("enableModels deletes (and persists the deletion of) a row that was never loaded", async () => {
    vi.resetModules();
    const repo = await import("@/lib/db/repos/disabledModelsRepo.js");
    await repo.enableModels("openai", []);
    await settle();
    expect(holder.d1.rowCount("kv")).toBe(2); // only the disabledModels row went
  });

  it("updatePricing merges into the persisted provider row instead of replacing it", async () => {
    vi.resetModules();
    const repo = await import("@/lib/db/repos/pricingRepo.js");
    await repo.updatePricing({ openai: { "gpt-y": { input: 3, output: 4 } } });
    await settle();
    const stored = JSON.parse(JSON.parse(holder.d1.raw.prepare("SELECT data FROM ninerouter_rows WHERE table_name='kv' AND row_key='scope:pricing|key:openai'").get().data).value);
    expect(Object.keys(stored).sort()).toEqual(["gpt-x", "gpt-y"]);
  });

  it("addCustomModel updates the persisted row instead of inserting a duplicate", async () => {
    vi.resetModules();
    const repo = await import("@/lib/db/repos/aliasRepo.js");
    const added = await repo.addCustomModel({ providerAlias: "openai", id: "m1", type: "llm", name: "Renamed" });
    expect(added).toBe(false);
  });

  it("exportDb includes connections and kv scopes from a cold isolate", async () => {
    seed(holder.d1, [["providerConnections", "id:c1", { id: "c1", provider: "openai", authType: "apikey", name: "n", email: null, priority: 1, isActive: 1, data: "{}", createdAt: "t", updatedAt: "t" }]]);
    vi.resetModules();
    const { exportDb } = await import("@/lib/db/index.js");
    const out = await exportDb();
    expect(out.providerConnections.map((c) => c.id)).toEqual(["c1"]);
    expect(out.customModels).toHaveLength(1);
    expect(Object.keys(out.pricing)).toEqual(["openai"]);
  });
});
