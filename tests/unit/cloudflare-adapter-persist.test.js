import { describe, it, expect, vi, beforeEach } from "vitest";

// Fake D1: records every executed statement so tests can assert write shape
// (row-level upserts vs whole-table DELETE + re-INSERT rewrites).
const calls = [];
const fakeD1 = {
  prepare: (sql) => ({
    bind: (...args) => ({
      run: async () => {
        calls.push({ sql, args });
        return {};
      },
      // Real D1 statements support .all() after .bind(); the scoped table read
      // relies on it.
      all: async () => {
        calls.push({ sql, args });
        return { results: [] };
      },
      first: async () => {
        calls.push({ sql, args });
        return null;
      },
    }),
    run: async () => {
      calls.push({ sql, args: [] });
      return {};
    },
    all: async () => {
      calls.push({ sql, args: [] });
      return { results: [] };
    },
  }),
  batch: async (stmts) => {
    for (const s of stmts || []) await s.run?.();
    return [];
  },
};

vi.mock("@opennextjs/cloudflare", () => ({
  getCloudflareContext: async () => ({ env: { DB: fakeD1 }, ctx: {} }),
}));

const { createCloudflareMemoryAdapter } = await import(
  "../../src/lib/db/adapters/cloudflareMemoryAdapter.js"
);

beforeEach(() => {
  calls.length = 0;
});

const flush = () => new Promise((r) => setTimeout(r, 50));
const wholeTableDelete = (table) =>
  calls.filter((c) => c.sql === "DELETE FROM ninerouter_rows WHERE table_name = ?" && c.args[0] === table);

describe("cloudflare D1 adapter transaction persists", () => {
  it("persists only touched rows, never whole-table rewrites", async () => {
    const db = await createCloudflareMemoryAdapter();
    calls.length = 0; // drop initializeD1 statements

    db.transaction(() => {
      for (let i = 0; i < 3; i++) {
        db.run(`INSERT INTO kv(scope, key, value) VALUES(?, ?, ?)`, [`s${i}`, `k${i}`, `v${i}`]);
      }
    });
    await flush();

    // No wholesale DELETE of the kv table...
    expect(wholeTableDelete("kv").length).toBe(0);
    // ...exactly 3 single-row upserts.
    const upserts = calls.filter((c) => c.sql.includes("ON CONFLICT(table_name, row_key)") && c.args[0] === "kv");
    expect(upserts.length).toBe(3);

    // Memory state is intact.
    expect(db.all(`SELECT * FROM kv`).length).toBe(3);
  });

  it("an update inside a transaction persists one row even with siblings present", async () => {
    const db = await createCloudflareMemoryAdapter();
    for (let i = 0; i < 5; i++) {
      db.run(`INSERT INTO kv(scope, key, value) VALUES(?, ?, ?)`, ["s", `k${i}`, `v${i}`]);
    }
    await flush();
    calls.length = 0;

    db.transaction(() => {
      db.run(`UPDATE kv SET value = ? WHERE scope = ? AND key = ?`, ["NEW", "s", "k2"]);
    });
    await flush();

    expect(wholeTableDelete("kv").length).toBe(0);
    const upserts = calls.filter((c) => c.sql.includes("ON CONFLICT(table_name, row_key)") && c.args[0] === "kv");
    expect(upserts.length).toBe(1);
    expect(db.get(`SELECT value FROM kv WHERE scope = ? AND key = ?`, ["s", "k2"]).value).toBe("NEW");
  });

  it("a delete inside a transaction persists row deletes, not a rewrite", async () => {
    const db = await createCloudflareMemoryAdapter();
    db.run(`INSERT INTO kv(scope, key, value) VALUES(?, ?, ?)`, ["s", "gone", "v"]);
    await flush();
    calls.length = 0;

    db.transaction(() => {
      db.run(`DELETE FROM kv WHERE scope = ? AND key = ?`, ["s", "gone"]);
    });
    await flush();

    expect(wholeTableDelete("kv").length).toBe(0);
    const dels = calls.filter(
      (c) => c.sql === "DELETE FROM ninerouter_rows WHERE table_name = ? AND row_key = ?" && c.args[0] === "kv",
    );
    expect(dels.length).toBe(1);
    expect(db.all(`SELECT * FROM kv`).length).toBe(0);
  });

  it("non-transactional writes still persist single rows", async () => {
    const db = await createCloudflareMemoryAdapter();
    calls.length = 0;

    db.run(`INSERT INTO _meta(key, value) VALUES(?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value`, [
      "k",
      "v",
    ]);
    await flush();

    expect(wholeTableDelete("_meta").length).toBe(0);
    expect(calls.filter((c) => c.args[0] === "_meta").length).toBe(1);
  });
});
