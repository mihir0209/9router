import { afterAll, beforeAll, describe, expect, it } from "vitest";

// Integration tests against a REAL PostgreSQL. Not a mock, not a fake dialect —
// the whole point of this adapter is that the SQL it emits is accepted by
// Postgres, and only a real server can tell you that.
//
// Skipped automatically when TEST_DATABASE_URL is unset, so the suite still runs
// for anyone without a database:
//
//   tests/helpers/pg.sh up
//   export TEST_DATABASE_URL=postgres://postgres:testpw@127.0.0.1:5433/n9router_test
//   npx vitest run unit/postgres-adapter.integration.test.js
// TEST_DATABASE_URL only, never DATABASE_URL. These tests drop and recreate
// the public schema, so honouring the app's own DATABASE_URL here would destroy
// whatever database someone is actually running against.
const connectionString = process.env.TEST_DATABASE_URL;
const describePg = connectionString ? describe : describe.skip;

describePg("postgres memory adapter (integration)", () => {
  let adapter;
  let createPostgresMemoryAdapter;

  beforeAll(async () => {
    ({ createPostgresMemoryAdapter } = await import(
      "../../src/lib/db/adapters/postgresMemoryAdapter.js"
    ));
    adapter = await createPostgresMemoryAdapter({ connectionString });
  });

  afterAll(async () => {
    if (adapter) await adapter.close();
  });

  const clearAll = () => {
    adapter.exec("DELETE FROM kv");
    adapter.exec("DELETE FROM combos");
    adapter.exec("DELETE FROM apiKeys");
    adapter.exec("DELETE FROM settings");
  };

  it("round-trips a row through Postgres", async () => {
    adapter.run(
      `INSERT INTO apiKeys (id, key, name, isActive, createdAt) VALUES (?, ?, ?, ?, ?)`,
      ["k1", "sk-pg-1", "pg key", 1, "2026-09-28"]
    );
    await adapter.flush();
    const row = adapter.get(`SELECT * FROM apiKeys WHERE id = ?`, ["k1"]);
    expect(row.name).toBe("pg key");
    expect(row.key).toBe("sk-pg-1");
  });

  it("survives a reload — the row is really in Postgres, not just in memory", async () => {
    // A second adapter over the same database proves durability, which is the
    // entire reason for choosing Postgres over a container-local file.
    const other = await createPostgresMemoryAdapter({ connectionString });
    const row = other.get(`SELECT * FROM apiKeys WHERE id = ?`, ["k1"]);
    expect(row?.name).toBe("pg key");
    await other.close();
  });

  it("updates in place, keyed on the primary key", async () => {
    adapter.run(`UPDATE apiKeys SET name = ? WHERE id = ?`, ["renamed", "k1"]);
    await adapter.flush();
    expect(adapter.get(`SELECT * FROM apiKeys WHERE id = ?`, ["k1"]).name).toBe("renamed");
  });

  it("upserts on conflict rather than duplicating", async () => {
    adapter.run(
      `INSERT INTO apiKeys (id, key, name, createdAt) VALUES (?, ?, ?, ?) ON CONFLICT (id) DO UPDATE SET name = excluded.name`,
      ["k1", "sk-pg-1", "upserted", "2026-09-28"]
    );
    await adapter.flush();
    const rows = adapter.all(`SELECT * FROM apiKeys WHERE id = ?`, ["k1"]);
    expect(rows).toHaveLength(1);
    expect(rows[0].name).toBe("upserted");
  });

  it("deletes", async () => {
    adapter.run(`DELETE FROM apiKeys WHERE id = ?`, ["k1"]);
    await adapter.flush();
    expect(adapter.get(`SELECT * FROM apiKeys WHERE id = ?`, ["k1"])).toBeUndefined();

    const other = await createPostgresMemoryAdapter({ connectionString });
    expect(other.get(`SELECT * FROM apiKeys WHERE id = ?`, ["k1"])).toBeUndefined();
    await other.close();
  });

  it("handles the composite primary key on kv", async () => {
    adapter.run(`INSERT INTO kv (scope, key, value) VALUES (?, ?, ?)`, [
      "modelAliases",
      "alias-a",
      '"openai/gpt-4o"',
    ]);
    await adapter.flush();
    const other = await createPostgresMemoryAdapter({ connectionString });
    // kv loads on first touch, not at boot — same contract the D1 adapter has,
    // and the reason the repo layer calls ensureTables before reading a scope.
    expect(other.raw.tables.kv).toEqual([]);
    await other.ensureTables(["kv"]);
    const row = other.get(`SELECT value FROM kv WHERE scope = ? AND key = ?`, [
      "modelAliases",
      "alias-a",
    ]);
    expect(row?.value).toBe('"openai/gpt-4o"');
    await other.close();
  });

  it("assigns autoincrement ids in-process and persists them", async () => {
    adapter.run(
      `INSERT INTO usageHistory (timestamp, provider, model, promptTokens, cost) VALUES (?, ?, ?, ?, ?)`,
      ["2026-09-28T00:00:00Z", "openai", "gpt-4o", 10, 0.01]
    );
    await adapter.flush();
    const row = adapter.get(`SELECT * FROM usageHistory ORDER BY id DESC LIMIT 1`);
    expect(typeof row.id).toBe("number");
    expect(row.cost).toBeCloseTo(0.01, 6);
  });

  it("preserves double precision on money", async () => {
    // SQLite REAL is 8 bytes, Postgres REAL is 4. If the column had been left as
    // REAL this would round and the budget would drift.
    const precise = 1234.5678901234;
    adapter.run(`INSERT INTO apiKeys (id, key, budgetUsd, createdAt) VALUES (?, ?, ?, ?)`, [
      "k-money",
      "sk-pg-money",
      precise,
      "2026-09-28",
    ]);
    await adapter.flush();
    const other = await createPostgresMemoryAdapter({ connectionString });
    const row = other.get(`SELECT budgetUsd FROM apiKeys WHERE id = ?`, ["k-money"]);
    expect(row.budgetUsd).toBeCloseTo(precise, 6);
    await other.close();
  });

  it("commits a multi-row transaction in one batch", async () => {
    adapter.transaction(() => {
      adapter.run(`INSERT INTO combos (id, name, models, createdAt, updatedAt) VALUES (?, ?, ?, ?, ?)`, [
        "c1",
        "combo-one",
        '["a"]',
        "2026-09-28",
        "2026-09-28",
      ]);
      adapter.run(`INSERT INTO combos (id, name, models, createdAt, updatedAt) VALUES (?, ?, ?, ?, ?)`, [
        "c2",
        "combo-two",
        '["b"]',
        "2026-09-28",
        "2026-09-28",
      ]);
    });
    await adapter.flush();
    const other = await createPostgresMemoryAdapter({ connectionString });
    expect(other.all(`SELECT * FROM combos`).map((r) => r.name).sort()).toEqual([
      "combo-one",
      "combo-two",
    ]);
    await other.close();
  });

  it("batches a transaction, and does NOT roll back on throw", async () => {
    // transaction() is a batching boundary, not an ACID transaction: it defers
    // persistence to the commit and has no undo log, so a throw still flushes
    // whatever the body wrote. That is long-standing behaviour shared with the
    // SQLite and D1 paths — it is pinned here so nobody "fixes" the Postgres
    // adapter into being the odd one out, and so the lack of rollback is a
    // decision on record rather than a surprise.
    expect(() =>
      adapter.transaction(() => {
        adapter.run(
          `INSERT INTO combos (id, name, models, createdAt, updatedAt) VALUES (?, ?, ?, ?, ?)`,
          ["c3", "combo-three", '["c"]', "2026-09-28", "2026-09-28"]
        );
        throw new Error("boom");
      })
    ).toThrow("boom");
    await adapter.flush();
    expect(adapter.get(`SELECT * FROM combos WHERE id = ?`, ["c3"])).toBeDefined();

    const other = await createPostgresMemoryAdapter({ connectionString });
    expect(other.get(`SELECT * FROM combos WHERE id = ?`, ["c3"])).toBeDefined();
    await other.close();
  });

  it("supports SUM/COUNT/DISTINCT aggregates the analytics code relies on", async () => {
    clearAll();
    await adapter.flush();
    for (const [i, id] of ["a1", "a2", "a3"].entries()) {
      adapter.run(`INSERT INTO apiKeys (id, key, budgetUsd, createdAt) VALUES (?, ?, ?, ?)`, [
        id,
        `sk-agg-${i}`,
        10.5,
        "2026-09-28",
      ]);
    }
    await adapter.flush();
    const sum = adapter.get(`SELECT COALESCE(SUM(budgetUsd), 0) AS total FROM apiKeys`);
    expect(sum.total).toBeCloseTo(31.5, 6);
    const count = adapter.get(`SELECT COUNT(*) AS n FROM apiKeys`);
    expect(count.n).toBe(3);
  });

  it("lazily loads a table on first touch", async () => {
    adapter.run(`INSERT INTO providerConnections (id, provider, authType, data, createdAt, updatedAt) VALUES (?, ?, ?, ?, ?, ?)`, [
      "pc1",
      "openai",
      "apikey",
      "{}",
      "2026-09-28",
      "2026-09-28",
    ]);
    await adapter.flush();
    const other = await createPostgresMemoryAdapter({ connectionString });
    expect(other.raw.tables.providerConnections).toEqual([]);
    await other.ensureTables(["providerConnections"]);
    expect(other.all(`SELECT * FROM providerConnections`)).toHaveLength(1);
    await other.close();
  });
});
