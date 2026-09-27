import { describe, it, expect, vi } from "vitest";

/**
 * On the Workers Free plan the CPU budget is 10ms per request. Cold start used
 * to fetch and JSON.parse the whole database — 1.28MB / 3,631 rows on the live
 * D1, of which kv was 919KB and providerConnections 295KB. D1 query time is not
 * billed as CPU, but JSON.parse is, so that parse is what blew the budget and
 * produced "Worker exceeded CPU time limit" on endpoints that read almost
 * nothing.
 *
 * These pin that the four large tables are no longer resident at init and are
 * pulled from D1 on first touch, with a concurrent second caller not triggering
 * a duplicate load.
 */
const holder = vi.hoisted(() => ({ d1: null, selects: [] }));
vi.mock("@opennextjs/cloudflare", () => ({
  getCloudflareContext: async () => ({ ctx: { waitUntil: () => {} }, env: { DB: holder.d1 } }),
}));

function makeD1() {
  return {
    prepare(sql) {
      return {
        run: async () => ({ success: true }),
        bind: (...values) => ({
          all: async () => {
            const m = /table_name IN \(([^)]*)\)/.exec(sql);
            const tables = m ? values : ["kv", "providerConnections", "usageHistory", "requestDetails"];
            holder.selects.push(sql);
            const rows = {
              kv: [{ scope: "modelAliases", key: "a", value: '{"x":1}' }],
              providerConnections: [{ id: "c1", provider: "openai" }],
              usageHistory: [{ id: 1, apiKey: "sk-1" }],
              requestDetails: [{ id: 1 }],
            };
            return { results: tables.flatMap((t) => (rows[t] || []).map((d) => ({ table_name: t, data: JSON.stringify(d) }))) };
          },
          run: async () => ({ success: true }),
        }),
        all: async () => ({ results: [] }),
      };
    },
    batch: async () => [],
  };
}

async function makeAdapter() {
  holder.d1 = makeD1();
  holder.selects.length = 0;
  vi.resetModules();
  const mod = await import("../../src/lib/db/adapters/cloudflareMemoryAdapter.js");
  return mod.createCloudflareMemoryAdapter();
}

const big = ["kv", "providerConnections", "usageHistory", "requestDetails"];

describe("cloudflare adapter — lazy tables", () => {
  it("does not load the big tables at isolate start", async () => {
    const db = await makeAdapter();
    const init = holder.selects[0] || "";
    for (const t of big) {
      expect(init).not.toContain(t);
    }
    // and they are genuinely empty in memory until touched
    expect(db.raw.tables.kv).toEqual([]);
    expect(db.raw.tables.providerConnections).toEqual([]);
    expect(db.raw.tables.usageHistory).toEqual([]);
    expect(db.raw.tables.requestDetails).toEqual([]);
  });

  it("still loads the small tables at start", async () => {
    const db = await makeAdapter();
    for (const t of ["settings", "apiKeys", "combos"]) {
      expect(db.raw.tables).toHaveProperty(t);
    }
  });

  it("loads a lazy table on first query and populates it", async () => {
    const db = await makeAdapter();
    await db.ensureTables(["kv"]);
    expect(db.raw.tables.kv).toHaveLength(1);
    expect(db.all("SELECT * FROM kv")).toHaveLength(1);
  });

  it("does not re-query a table it already loaded", async () => {
    const db = await makeAdapter();
    await db.ensureTables(["providerConnections"]);
    const after = holder.selects.length;
    await db.ensureTables(["providerConnections"]);
    await db.ensureTables(["providerConnections"]);
    expect(holder.selects.length).toBe(after);
  });

  it("a concurrent first touch does not duplicate rows", async () => {
    // Three callers racing on a cold table must end up with the table loaded
    // once. Deduplication is by marking loaded before awaiting; the observable
    // consequence is one row, not three.
    const db = await makeAdapter();
    await Promise.all([
      db.ensureTables(["usageHistory"]),
      db.ensureTables(["usageHistory"]),
      db.ensureTables(["usageHistory"]),
    ]);
    expect(db.raw.tables.usageHistory).toHaveLength(1);
  });

  it("a failed load is not cached as loaded", async () => {
    const db = await makeAdapter();
    const failing = { ...makeD1(), prepare: () => ({ run: async () => ({}), bind: () => ({ all: async () => { throw new Error("D1 down"); }, run: async () => ({}) }), all: async () => ({ results: [] }) }) };
    holder.d1 = failing;
    await db.ensureTables(["requestDetails"]);
    // Retry must be allowed rather than silently returning empty forever.
    holder.d1 = makeD1();
    await db.ensureTables(["requestDetails"]);
    expect(db.raw.tables.requestDetails).toHaveLength(1);
  });
});
