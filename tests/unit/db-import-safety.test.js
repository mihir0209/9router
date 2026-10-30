import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { DatabaseSync } from "node:sqlite";

/**
 * 2026-10-30: a dashboard "Import Backup" wiped the whole Providers section on the Cloudflare
 * deployment. The D1 showed ~3,100 row deletes and no insert at all. Causes pinned here:
 *
 *  1. The page posted `{ ...mergeBackups(...), password }`. mergeBackups returns the wrapper
 *     `{ merged, stats, errors }`, so the body carried no table; importDb (replace semantics) then
 *     deleted everything and inserted nothing, and the route answered 200.
 *  2. importDb accepted any object, including one with no table in it.
 *  3. On Cloudflare the route could not wait for D1 (no adapter flush()), deletes were sent before
 *     the new rows, and failed writes were only logged.
 */

const holder = vi.hoisted(() => ({ d1: null }));
vi.mock("@opennextjs/cloudflare", () => ({
  getCloudflareContext: () => ({ ctx: { waitUntil: () => {} }, env: { DB: holder.d1 } }),
}));
vi.mock("@/lib/coordinator/controlVersion.js", () => ({
  bumpControlVersion: async () => {},
  getControlVersion: async () => null,
}));

const now = new Date().toISOString();
const conn = (id) => ({ id, provider: "openai", authType: "apikey", name: id, apiKey: "sk-" + id, isActive: true, createdAt: now, updatedAt: now });
const backup = (ids) => ({
  settings: { requireApiKey: false },
  providerConnections: ids.map(conn),
  apiKeys: [], combos: [], providerNodes: [], proxyPools: [],
  modelAliases: { a: "openai/gpt-4o" }, customModels: [], mitmAlias: {}, pricing: {},
});

function makeD1({ delayMs = 0, failAfter = Infinity } = {}) {
  const sqlite = new DatabaseSync(":memory:");
  sqlite.exec(`CREATE TABLE ninerouter_rows (table_name TEXT NOT NULL, row_key TEXT NOT NULL, data TEXT NOT NULL, updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, PRIMARY KEY (table_name, row_key))`);
  const log = [];
  let executed = 0;
  const sleep = () => (delayMs ? new Promise((r) => setTimeout(r, delayMs)) : Promise.resolve());
  const stmt = (sql, params = []) => ({
    sql, params,
    bind: (...p) => stmt(sql, p),
    run: async () => { await sleep(); const r = sqlite.prepare(sql).run(...params); return { success: true, meta: { changes: r.changes } }; },
    all: async () => ({ results: sqlite.prepare(sql).all(...params) }),
    first: async () => sqlite.prepare(sql).get(...params) ?? null,
  });
  return {
    sqlite, log,
    prepare: (sql) => stmt(sql),
    batch: async (stmts) => {
      await sleep();
      sqlite.exec("BEGIN");
      try {
        for (const s of stmts) {
          if (++executed > failAfter) throw new Error("D1 unavailable");
          log.push(`${/^DELETE/i.test(s.sql) ? "delete" : "upsert"}:${s.params[0]}`);
          sqlite.prepare(s.sql).run(...s.params);
        }
        sqlite.exec("COMMIT");
      } catch (e) { sqlite.exec("ROLLBACK"); throw e; }
      return stmts.map(() => ({ success: true }));
    },
  };
}
const count = (d1, table) => d1.sqlite.prepare("SELECT COUNT(*) n FROM ninerouter_rows WHERE table_name = ?").get(table).n;

async function load(d1) {
  holder.d1 = d1;
  process.env.NINEROUTER_RUNTIME = "cloudflare";
  delete global._dbAdapter;
  vi.resetModules();
  const db = await import("../../src/lib/db/index.js");
  const { getAdapter } = await import("../../src/lib/db/driver.js");
  return { db, adapter: await getAdapter() };
}

let originalRuntime;
beforeEach(() => { originalRuntime = process.env.NINEROUTER_RUNTIME; });
afterEach(() => {
  delete global._dbAdapter;
  if (originalRuntime === undefined) delete process.env.NINEROUTER_RUNTIME;
  else process.env.NINEROUTER_RUNTIME = originalRuntime;
});

describe("merge import payload (dashboard)", () => {
  it("is the merged tables, not the { merged, stats, errors } wrapper", async () => {
    const { buildMergeImportPayload } = await import("../../src/lib/db/mergeBackups.js");
    const payload = buildMergeImportPayload(backup(["a"]), backup(["a", "b"]));
    expect(Object.keys(payload)).not.toContain("merged");
    expect(Object.keys(payload)).not.toContain("stats");
    expect(Object.keys(payload)).not.toContain("errors");
    expect(payload.providerConnections.map((c) => c.id).sort()).toEqual(["a", "b"]);
    // exactly what the page sends
    const body = JSON.parse(JSON.stringify({ ...payload, password: "x" }));
    expect(body.providerConnections).toHaveLength(2);
  });

  it("throws instead of producing an empty payload when a table is null", async () => {
    const { buildMergeImportPayload } = await import("../../src/lib/db/mergeBackups.js");
    expect(() => buildMergeImportPayload(backup(["a"]), { ...backup(["b"]), pricing: null })).toThrow(/nothing was changed/);
  });
});

describe("importDb refuses a payload with no table (Cloudflare adapter)", () => {
  for (const [label, payload] of [["{}", {}], ["wrapper keys only", { merged: {}, stats: {}, errors: [] }], ["unknown keys", { foo: 1 }], ["wrong shape", { providerConnections: {} }]]) {
    it(`rejects ${label} and deletes nothing`, async () => {
      const d1 = makeD1();
      const { db, adapter } = await load(d1);
      await db.importDb(backup(["old1", "old2"]));
      await adapter.flush();
      expect(count(d1, "providerConnections")).toBe(2);
      d1.log.length = 0;

      await expect(db.importDb(payload)).rejects.toThrow(/Invalid database payload/);
      await adapter.flush();
      expect(d1.log).toEqual([]);
      expect(count(d1, "providerConnections")).toBe(2);
    });
  }
});

describe("Cloudflare adapter durability", () => {
  it("flush() resolves only after every D1 write of the import finished", async () => {
    const d1 = makeD1({ delayMs: 15 });
    const { db, adapter } = await load(d1);
    const ids = Array.from({ length: 120 }, (_, i) => `c${i}`);
    await db.importDb(backup(ids));
    // writes are still running in the background at this point
    expect(count(d1, "providerConnections")).toBeLessThan(120);
    await adapter.flush();
    expect(count(d1, "providerConnections")).toBe(120);
    expect(count(d1, "settings")).toBe(1);
    expect(count(d1, "kv")).toBe(1);
  });

  it("sends new rows before deleting old ones", async () => {
    const d1 = makeD1();
    const { db, adapter } = await load(d1);
    await db.importDb(backup(["old1", "old2", "old3"]));
    await adapter.flush();
    d1.log.length = 0;

    await db.importDb(backup(["new1", "new2"]));
    await adapter.flush();
    const rows = d1.sqlite.prepare("SELECT row_key FROM ninerouter_rows WHERE table_name = 'providerConnections' ORDER BY 1").all().map((r) => r.row_key);
    expect(rows).toEqual(["id:new1", "id:new2"]);
    expect(d1.log).toContain("delete:providerConnections");
    expect(d1.log.lastIndexOf("upsert:providerConnections")).toBeLessThan(d1.log.indexOf("delete:providerConnections"));
  });

  it("a write cut off after the first batch leaves the old rows, not an empty table", async () => {
    const d1 = makeD1();
    const { db, adapter } = await load(d1);
    await db.importDb(backup(["old1", "old2", "old3"]));
    await adapter.flush();
    expect(count(d1, "providerConnections")).toBe(3);

    // D1 goes away for every later statement: the new rows cannot be written
    const raw = d1.batch;
    d1.batch = async () => { throw new Error("D1 unavailable"); };
    await db.importDb(backup(["new1", "new2"]));
    await expect(adapter.flush()).rejects.toThrow(/D1 write did not complete/);
    d1.batch = raw;
    expect(count(d1, "providerConnections")).toBe(3); // old rows still there
  });

  it("flush() reports a failed write instead of letting the import look successful", async () => {
    const d1 = makeD1({ failAfter: 0 });
    const { db, adapter } = await load(d1);
    await db.importDb(backup(["a"]));
    await expect(adapter.flush()).rejects.toThrow(/D1 write did not complete/);
    // reported once, then clean
    await expect(adapter.flush()).resolves.toBeUndefined();
  });
});
