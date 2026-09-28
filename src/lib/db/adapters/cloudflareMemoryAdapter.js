import {
  PRIMARY_KEYS,
  AUTOINCREMENT,
  primaryKeyFor,
  clone,
  normalizeSql,
  splitCsv,
  literalValue,
  compare,
  buildWhere,
  parseSelect,
  projectRows,
  createState,
  createInMemoryOps,
} from "./memoryEngine.js";
import { TABLES } from "../schema.js";

const D1_TABLE = "ninerouter_rows";


async function getD1Context() {
  try {
    const { getCloudflareContext } = await import("@opennextjs/cloudflare");
    return await getCloudflareContext({ async: true });
  } catch (error) {
    console.warn(`[DB] Cloudflare D1 context unavailable: ${error.message}`);
    return null;
  }
}

export async function createCloudflareMemoryAdapter() {
  const state = createState();
  const context = await getD1Context();
  const d1 = context?.env?.DB || null;
  // Row-level dirty tracking inside transactions: table -> Map(rowKey, "upsert"|"delete").
  // Commit persists ONLY touched rows. Previously every transaction rewrote each
  // touched table wholesale (DELETE all + re-INSERT all), so a kv model sync
  // cost ~7000 D1 row writes and a requestDetails save ~700. On metered D1
  // (100k writes/day) that amplification is the budget killer.

  // Tables whose row count is bounded by configuration rather than by traffic.
  // usageHistory and requestDetails grow by one persisted row per request, so
  // eagerly loading them meant every cold isolate read and JSON.parsed the entire
  // request history before serving anything — which exhausted the Worker's
  // resource limits (CF 1102) and made the instance fail most requests. They are
  // analytics: nothing on the request path needs them resident, and an explicit
  // resync(force, ["usageHistory"]) can still pull them on demand.
  // Cold start used to fetch and JSON.parse the whole database before serving
  // anything: 1.28MB / 3,631 rows on the live D1, of which kv alone is 919KB and
  // providerConnections 295KB. On the Workers Free plan the CPU budget is 10ms
  // per request (Paid is 30s by default) and Cloudflare does not count time
  // spent waiting on the D1 query — only the JSON.parse. So the parse was
  // blowing the budget on nearly every cold isolate, which is the
  // "Worker exceeded CPU time limit" on endpoints that read almost nothing.
  //
  // These tables therefore load on first touch instead of at init. kv is
  // reference data most requests never read; providerConnections is read by
  // routing, not by the admin/auth surface; the other two are analytics that
  // belong in the DB, not in a Worker's heap.
  const LAZY_TABLES = new Set(["kv", "providerConnections", "usageHistory", "requestDetails"]);
  const EAGER_TABLES = Object.keys(TABLES).filter((table) => !LAZY_TABLES.has(table));
  const loadedTables = new Set(EAGER_TABLES);

  /** Load a lazy table from D1 once, on first touch. Safe to call concurrently. */
  async function ensureTables(names) {
    if (!d1) return;
    const wanted = (Array.isArray(names) ? names : [names]).filter((n) => LAZY_TABLES.has(n) && !loadedTables.has(n));
    if (!wanted.length || ops?.inTransaction()) return;
    // Mark before awaiting so a concurrent caller cannot double-load.
    for (const table of wanted) loadedTables.add(table);
    try {
      const fresh = await fetchTablesFromD1(wanted);
      for (const table of wanted) {
        if (!state.tables[table]) state.tables[table] = [];
        for (const row of fresh[table] || []) state.tables[table].push(row);
      }
    } catch (error) {
      for (const table of wanted) loadedTables.delete(table);
      console.warn(`[DB] lazy load failed for ${wanted.join(",")}: ${error.message}`);
    }
  }

  /**
   * Read the given tables into a scratch object. Live state is never touched
   * here — the caller decides what to swap in — so a table with uncommitted
   * local rows keeps them.
   */
  async function fetchTablesFromD1(tableNames) {
    const fresh = {};
    for (const table of tableNames) fresh[table] = [];
    if (!d1 || !tableNames.length) return fresh;
    const result = tableNames.length === 1 && tableNames[0] === "*"
      ? await d1.prepare(`SELECT table_name, data FROM ${D1_TABLE}`).all()
      : await d1
          .prepare(
            `SELECT table_name, data FROM ${D1_TABLE} WHERE table_name IN (${tableNames.map(() => "?").join(", ")})`
          )
          .bind(...tableNames)
          .all();
    for (const row of result.results || []) {
      if (!fresh[row.table_name]) continue;
      try {
        const parsed = JSON.parse(row.data);
        fresh[row.table_name].push(parsed);
        if (AUTOINCREMENT.has(row.table_name) && Number.isFinite(Number(parsed.id))) {
          state.sequences[row.table_name] = Math.max(state.sequences[row.table_name] || 0, Number(parsed.id));
        }
      } catch {
        // Ignore corrupt persisted rows; the in-memory adapter is fail-open.
      }
    }
    return fresh;
  }

  async function initializeD1() {
    if (!d1) return;
    await d1.prepare(
      `CREATE TABLE IF NOT EXISTS ${D1_TABLE} (` +
      `table_name TEXT NOT NULL, ` +
      `row_key TEXT NOT NULL, ` +
      `data TEXT NOT NULL, ` +
      `updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, ` +
      `PRIMARY KEY (table_name, row_key))`
    ).run();
    const fresh = await fetchTablesFromD1(EAGER_TABLES);
    for (const table of EAGER_TABLES) {
      state.tables[table] = fresh[table] || [];
    }
  }

  let lastD1SyncTs = Date.now();
  const D1_RESYNC_INTERVAL_MS = 5 * 60 * 1000;

  /**
   * Refresh in-memory tables from D1.
   *
   * `tableNames` scopes the read to just those tables. This matters a great
   * deal: usageHistory and requestDetails are persisted one D1 row per record,
   * so ninerouter_rows grows without bound as traffic accumulates, and a
   * full-table SELECT then has to read and JSON.parse every usage record ever
   * written. On a cold isolate that exhausts the Worker's resource limits
   * (CF 1102) — which is exactly what an unscoped forced resync on a hot path
   * caused. Callers that know which table they need must pass it.
   */
  async function resyncFromD1(force = false, tableNames = null) {
    if (!d1) return;
    const now = Date.now();
    const scoped = Array.isArray(tableNames) ? tableNames.filter((name) => TABLES[name]) : null;
    if (!force && now - lastD1SyncTs < D1_RESYNC_INTERVAL_MS) return;
    lastD1SyncTs = now;
    // Deduped: loadedTables is seeded with EAGER_TABLES, so a plain concat
    // would list the same table twice.
    const wanted = scoped || [...new Set([...loadedTables, ...EAGER_TABLES])];
    // Snapshot each table's row count. Anything awaited below yields to other
    // requests, and one of them may write to a table we are about to replace.
    // A table whose length moved while we were fetching has been written to and
    // must not be overwritten. Clearing state up front instead would silently
    // drop writes that only ever existed in memory.
    const before = new Map(wanted.map((table) => [table, (state.tables[table] || []).length]));
    try {
      const fresh = await fetchTablesFromD1(wanted);
      for (const table of wanted) {
        if (ops?.hasDirty(table) || ops?.inTransaction()) continue;
        // Rows are pushed in place, so the array identity is unchanged by a
        // concurrent write; its length is what moves.
        if ((state.tables[table] || []).length !== before.get(table)) continue; // written to mid-fetch
        state.tables[table] = fresh[table] || [];
      }
    } catch (error) {
      console.warn(`[DB] D1 resync failed: ${error.message}`);
    }
  }

  function persistTable(table) {
    if (!d1 || !state.tables[table]) return;
    if (ops?.inTransaction()) {
      ops.markDirty(table);
      return;
    }
    const rows = state.tables[table].map((row) => ({ key: primaryKeyFor(table, row), data: JSON.stringify(row) }));
    const task = (async () => {
      await d1.prepare(`DELETE FROM ${D1_TABLE} WHERE table_name = ?`).bind(table).run();
      const chunkSize = 50;
      // Persist in small batches to stay comfortably below D1 statement limits.
      for (let i = 0; i < rows.length; i += chunkSize) {
        await d1.batch(rows.slice(i, i + chunkSize).map((row) =>
          d1.prepare(
            `INSERT INTO ${D1_TABLE}(table_name, row_key, data, updated_at) ` +
            `VALUES(?, ?, ?, CURRENT_TIMESTAMP) ` +
            `ON CONFLICT(table_name, row_key) DO UPDATE SET data = excluded.data, updated_at = excluded.updated_at`
          ).bind(table, row.key, row.data)
        ));
      }
    })().catch((error) => console.warn(`[DB] D1 persist failed for ${table}: ${error.message}`));
    context?.ctx?.waitUntil?.(task);
  }

  function persistRow(table, row) {
    if (!d1 || !state.tables[table]) return;
    if (ops?.markDirty(table, row, "upsert")) return;
    const task = d1.prepare(
      `INSERT INTO ${D1_TABLE}(table_name, row_key, data, updated_at) ` +
      `VALUES(?, ?, ?, CURRENT_TIMESTAMP) ` +
      `ON CONFLICT(table_name, row_key) DO UPDATE SET data = excluded.data, updated_at = excluded.updated_at`
    ).bind(table, primaryKeyFor(table, row), JSON.stringify(row)).run()
      .catch((error) => console.warn(`[DB] D1 row persist failed for ${table}: ${error.message}`));
    context?.ctx?.waitUntil?.(task);
  }

  function deletePersistedRows(table, rows) {
    if (!d1 || !state.tables[table] || !rows.length) return;
    if (ops?.inTransaction()) {
      for (const row of rows) ops.markDirty(table, row, "delete");
      return;
    }
    const task = d1.batch(rows.map((row) =>
      d1.prepare(`DELETE FROM ${D1_TABLE} WHERE table_name = ? AND row_key = ?`)
        .bind(table, primaryKeyFor(table, row))
    )).catch((error) => console.warn(`[DB] D1 row delete failed for ${table}: ${error.message}`));
    context?.ctx?.waitUntil?.(task);
  }

  // Shared in-memory engine. D1 supplies only the persistence hooks; the SQL
  // interpretation and transaction semantics are the same code Postgres runs.
  const ops = createInMemoryOps(state, {
    persistRow,
    deletePersistedRows,
    persistDirtyRows: (table) => persistDirtyRows(table),
  });

  await initializeD1();

  // Commit path: persist only rows touched inside the transaction. Tables
  // with no row-level record (defensive; all writers record rows) fall back
  // to the legacy whole-table rewrite.
  function persistDirtyRows(table) {
    if (!d1 || !state.tables[table]) {
      ops?.pendingDirtyRows().delete(table);
      return;
    }
    const touched = ops.pendingDirtyRows().get(table);
    ops.pendingDirtyRows().delete(table);
    if (!touched || !touched.size) {
      persistTable(table);
      return;
    }
    const upserts = [];
    const deletes = [];
    for (const [key, op] of touched) {
      if (op === "delete") {
        deletes.push(key);
        continue;
      }
      const row = ops.rowsFor(table).find((r) => primaryKeyFor(table, r) === key);
      if (row) upserts.push(row);
      else deletes.push(key);
    }
    const task = (async () => {
      if (deletes.length) {
        await d1.batch(deletes.map((key) =>
          d1.prepare(`DELETE FROM ${D1_TABLE} WHERE table_name = ? AND row_key = ?`).bind(table, key)
        ));
      }
      const chunkSize = 50;
      for (let i = 0; i < upserts.length; i += chunkSize) {
        await d1.batch(upserts.slice(i, i + chunkSize).map((row) =>
          d1.prepare(
            `INSERT INTO ${D1_TABLE}(table_name, row_key, data, updated_at) ` +
            `VALUES(?, ?, ?, CURRENT_TIMESTAMP) ` +
            `ON CONFLICT(table_name, row_key) DO UPDATE SET data = excluded.data, updated_at = excluded.updated_at`
          ).bind(table, primaryKeyFor(table, row), JSON.stringify(row))
        ));
      }
    })().catch((error) => console.warn(`[DB] D1 persist failed for ${table}: ${error.message}`));
    context?.ctx?.waitUntil?.(task);
  }

  return {
    driver: d1 ? "cloudflare-d1-backed-memory" : "cloudflare-memory-js",
    run: ops.run,
    get: ops.get,
    all: ops.all,
    exec: ops.exec,
    transaction: ops.transaction,
    resync: resyncFromD1,
    ensureTables,
    close: () => {},
    raw: state,
  };
}
