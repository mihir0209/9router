import { describe, it, expect, beforeEach, vi } from "vitest";

// createCloudflareMemoryAdapter() resolves D1 via getCloudflareContext(), so the
// seam to mock is the module, not the factory's arguments.
const holder = vi.hoisted(() => ({ d1: null }));
vi.mock("@opennextjs/cloudflare", () => ({
  getCloudflareContext: async () => ({ ctx: { waitUntil: () => {} }, env: { DB: holder.d1 } }),
}));

/**
 * A forced, unscoped D1 resync reads EVERY row of ninerouter_rows and JSON.parses
 * each one. usageHistory and requestDetails are persisted one D1 row per record,
 * so that set grows without bound as traffic accumulates — and on a cold isolate
 * it exhausts the Worker's resource limits (CF 1102). This is what made
 * GET /api/admin/keys fail ~2 runs in 3 while POST still worked.
 *
 * The fix scopes the resync to the table the caller needs. These tests pin both
 * halves: the scoped read touches only its own table, and it still sees a write
 * made by another isolate (the correctness property the scoping must not cost).
 */

const calls = [];
let preparedSql = [];
let rowsByTable = {};

function makeD1() {
  return {
    prepare(sql) {
      preparedSql.push(sql);
      const match = /table_name IN \(([^)]*)\)/.exec(sql);
      return {
        run: async () => ({ success: true }),
        bind: (...values) => ({
          all: async () => {
            calls.push({ sql, values });
            const tables = match
              ? values
              : Object.keys(rowsByTable);
            return {
              results: tables.flatMap((t) =>
                (rowsByTable[t] || []).map((data) => ({ table_name: t, data: JSON.stringify(data) }))
              ),
            };
          },
          run: async () => ({ success: true }),
        }),
        all: async () => {
          calls.push({ sql, values: null });
          return {
            results: Object.keys(rowsByTable).flatMap((t) =>
              (rowsByTable[t] || []).map((data) => ({ table_name: t, data: JSON.stringify(data) }))
            ),
          };
        },
      };
    },
  };
}

async function makeAdapter(d1) {
  holder.d1 = d1;
  vi.resetModules();
  const mod = await import("../../src/lib/db/adapters/cloudflareMemoryAdapter.js");
  return mod.createCloudflareMemoryAdapter();
}

let d1;
let adapter;

beforeEach(() => {
  calls.length = 0;
  preparedSql.length = 0;
  // 500 usage rows is modest; production has tens of thousands.
  rowsByTable = {
    settings: [{ id: 1, data: '{"gatewayControl":{"disabled":false}}' }],
    apiKeys: [
      { id: "k1", key: "sk-a", name: "a", ownerRef: "t1", isActive: true },
      { id: "k2", key: "sk-b", name: "b", ownerRef: "t2", isActive: true },
    ],
    usageHistory: Array.from({ length: 500 }, (_, i) => ({
      id: i + 1, provider: "openai", model: "gpt-4o", cost: 0.01, apiKey: "sk-a",
    })),
    requestDetails: Array.from({ length: 200 }, (_, i) => ({ id: i + 1, status: "success" })),
  };
  d1 = makeD1();
});

describe("cloudflare memory adapter — scoped resync", () => {
  it("reads only the requested table, not every table", async () => {
    adapter = await makeAdapter(d1);
    await adapter.resync(true, ["apiKeys"]);

    const select = calls.filter((c) => c.values).at(-1);
    expect(select.sql).toContain("WHERE table_name IN");
    expect(select.values).toEqual(["apiKeys"]);
  });

  it("never touches usageHistory when scoped to another table", async () => {
    adapter = await makeAdapter(d1);
    await adapter.resync(true, ["apiKeys"]);

    // The regression: an unscoped scan would have pulled 500 usage rows and 200
    // request-detail rows through JSON.parse on a cold isolate.
    const returned = calls.filter((c) => c.values).at(-1).values;
    expect(returned).not.toContain("usageHistory");
    expect(returned).not.toContain("requestDetails");
  });

  it("leaves the unbounded tables unloaded rather than resident", async () => {
    adapter = await makeAdapter(d1);
    await adapter.resync(true, ["apiKeys"]);

    // The point of the fix: the per-request tables stay out of isolate memory.
    // (Reads of them need an explicit scoped resync — see the follow-up note.)
    expect(adapter.raw.tables.usageHistory).toEqual([]);
    expect(adapter.raw.tables.requestDetails).toEqual([]);
    // ...while the tables that matter for request handling are resident.
    expect(adapter.raw.tables.apiKeys).toHaveLength(2);
  });

  it("still loads its own table correctly", async () => {
    adapter = await makeAdapter(d1);
    await adapter.resync(true, ["apiKeys"]);
    const rows = adapter.all("SELECT * FROM apiKeys");
    expect(rows).toHaveLength(2);
    expect(rows[0].key).toBe("sk-a");
  });

  it("scoping to several tables reads exactly those", async () => {
    adapter = await makeAdapter(d1);
    await adapter.resync(true, ["apiKeys", "settings"]);
    expect(calls.filter((c) => c.values).at(-1).values.sort()).toEqual(["apiKeys", "settings"]);
  });

  it("ignores an unknown table name rather than building an empty IN clause", async () => {
    adapter = await makeAdapter(d1);
    await adapter.resync(true, ["notATable"]);
    // Falls back rather than emitting invalid SQL.
    expect(calls.every((c) => !c.sql.includes("IN ()"))).toBe(true);
  });

  it("an unscoped resync loads the eager tables and still excludes the unbounded ones", async () => {
    adapter = await makeAdapter(d1);
    await adapter.resync(true);
    const values = calls.filter((c) => c.values).at(-1).values;
    expect(values).toContain("apiKeys");
    // The regression guard: even with no explicit scope, the two per-request
    // tables must never be pulled.
    expect(values).not.toContain("usageHistory");
    expect(values).not.toContain("requestDetails");
    expect(adapter.all("SELECT * FROM apiKeys")).toHaveLength(2);
  });

  it("cold isolate creation does not load usageHistory or requestDetails", async () => {
    // This is the pre-existing half of the 1102: initializeD1 ran an unfiltered
    // SELECT on every cold isolate, before any request code at all.
    calls.length = 0;
    adapter = await makeAdapter(d1);
    const values = calls.filter((c) => c.values).at(-1).values;
    expect(values).not.toContain("usageHistory");
    expect(values).not.toContain("requestDetails");
    expect(values).toContain("settings");
  });

  it("still allows an explicit on-demand load of an unbounded table", async () => {
    adapter = await makeAdapter(d1);
    await adapter.resync(true, ["usageHistory"]);
    expect(calls.filter((c) => c.values).at(-1).values).toEqual(["usageHistory"]);
    expect(adapter.all("SELECT * FROM usageHistory")).toHaveLength(500);
  });
});
