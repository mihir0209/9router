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
import { createFlightGroup, withOwnTimeout } from "../../../../open-sse/utils/flight.js";

const D1_TABLE = "ninerouter_rows";


// Every D1 call is bounded. A D1 query that never answers (or whose request was cancelled)
// must not hold a request for minutes: the timer is armed by the caller, in its own request
// context. Auth and enforcement paths treat the rejection as fail-closed (they already do for
// any D1 error); writes warn and are retried by the next flush.
const D1_TIMEOUT_MS = () => {
  const n = Number(process.env.NINEROUTER_D1_TIMEOUT_MS);
  return Number.isFinite(n) && n >= 0 ? n : 8000;
};

export function guardD1(raw, getMs = D1_TIMEOUT_MS) {
  if (!raw) return raw;
  const unwrap = (stmt) => (stmt && stmt.__raw ? stmt.__raw : stmt);
  const wrapStmt = (stmt) => ({
    __raw: stmt,
    bind: (...args) => wrapStmt(stmt.bind(...args)),
    all: (...args) => withOwnTimeout(stmt.all(...args), getMs(), "D1 query"),
    run: (...args) => withOwnTimeout(stmt.run(...args), getMs(), "D1 query"),
    first: (...args) => withOwnTimeout(stmt.first(...args), getMs(), "D1 query"),
    raw: (...args) => withOwnTimeout(stmt.raw(...args), getMs(), "D1 query"),
  });
  return {
    prepare: (sql) => wrapStmt(raw.prepare(sql)),
    batch: (stmts) => withOwnTimeout(raw.batch((stmts || []).map(unwrap)), getMs(), "D1 batch"),
    exec: (sql) => withOwnTimeout(raw.exec(sql), getMs(), "D1 exec"),
  };
}

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
  const d1 = guardD1(context?.env?.DB || null);
  // waitUntil of the CURRENT request. The context captured at boot belongs to the request that
  // happened to create the adapter; once that request has finished, its waitUntil no longer
  // keeps later writes alive.
  let cfModule = null;
  try { cfModule = await import("@opennextjs/cloudflare"); } catch { /* not on Workers */ }
  // Every background D1 write is tracked so a caller can wait for durability (flush()).
  // Writes are applied to memory synchronously and persisted afterwards; without a way to wait,
  // a request such as a database restore answered 200 while its rows were still being written, and
  // Workers cancel waitUntil work about 30 s after the response.
  const pendingWrites = new Set();
  let failedWrites = 0;
  let reportedFailures = 0;
  let lastWriteFailure = "";
  function noteWriteFailure(label, error) {
    failedWrites++;
    lastWriteFailure = `${label}: ${error?.message || error}`;
    console.warn(`[DB] ${lastWriteFailure}`);
  }
  function keepAlive(task) {
    pendingWrites.add(task);
    Promise.resolve(task).finally(() => pendingWrites.delete(task)).catch(() => {});
    try {
      const current = cfModule?.getCloudflareContext?.();
      if (current?.ctx?.waitUntil) { current.ctx.waitUntil(task); return; }
    } catch { /* outside a request: fall back to the boot context */ }
    try { context?.ctx?.waitUntil?.(task); } catch { /* best effort */ }
  }
  // Loads shared between concurrent requests are request-safe flights (see open-sse/utils/flight.js).
  const loadFlights = createFlightGroup({ followerWaitMs: 3000, staleMs: 20000 });
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
  // usageDaily is lazy too: one fat row per calendar day, read only by the usage
  // dashboard; the analytics flush point-loads the single day it merges into
  // (ensureRow) instead of every cold isolate reading the whole history.
  const LAZY_TABLES = new Set(["kv", "providerConnections", "usageHistory", "requestDetails", "usageDaily"]);
  const EAGER_TABLES = Object.keys(TABLES).filter((table) => !LAZY_TABLES.has(table));
  const loadedTables = new Set(EAGER_TABLES);

  // Last time each table was read from D1. Lets callers skip a forced refresh
  // that would re-read what the isolate fetched a moment ago (cold start loads
  // settings and apiKeys, and the first request then used to force-resync both).
  const fetchedAt = new Map();
  function freshWithin(table, ms) {
    const at = fetchedAt.get(table);
    return at !== undefined && Date.now() - at < ms;
  }

  /** Push fetched rows into live state, skipping rows already resident (local writes win). */
  function mergeFetched(table, rows) {
    if (!state.tables[table]) state.tables[table] = [];
    const resident = state.tables[table];
    if (!resident.length) {
      for (const row of rows) resident.push(row);
      return;
    }
    const known = new Set(resident.map((row) => primaryKeyFor(table, row)));
    for (const row of rows) if (!known.has(primaryKeyFor(table, row))) resident.push(row);
  }

  /**
   * Load a lazy table from D1 once, on first touch. Safe to call concurrently.
   * A table counts as loaded only after its rows are resident (it used to be marked before the
   * await, so a concurrent request proceeded on an empty table). Concurrent callers share one load
   * through a flight and never wait on it unboundedly.
   */
  async function ensureTables(names) {
    if (!d1) return;
    const wanted = (Array.isArray(names) ? names : [names]).filter((n) => LAZY_TABLES.has(n) && !loadedTables.has(n));
    if (!wanted.length || ops?.inTransaction()) return;
    try {
      await loadFlights.run(`tables:${[...wanted].sort().join(",")}`, async () => {
        const todo = wanted.filter((table) => !loadedTables.has(table));
        if (!todo.length) return;
        const fresh = await fetchTablesFromD1(todo);
        for (const table of todo) {
          if (loadedTables.has(table)) continue;
          mergeFetched(table, fresh[table] || []);
          loadedTables.add(table);
        }
      });
    } catch (error) {
      console.warn(`[DB] lazy load failed for ${wanted.join(",")}: ${error.message}`);
    }
  }

  /** Point-load one persisted row of a lazy table (no-op once the table is fully resident). */
  const loadedRows = new Set();
  async function ensureRow(table, rowKey) {
    if (!d1 || !LAZY_TABLES.has(table) || loadedTables.has(table) || ops?.inTransaction()) return;
    const id = `${table}\u0000${rowKey}`;
    if (loadedRows.has(id)) return;
    try {
      await loadFlights.run(`row:${id}`, async () => {
        const result = await d1.prepare(`SELECT table_name, data FROM ${D1_TABLE} WHERE table_name = ? AND row_key = ?`).bind(table, rowKey).all();
        const rows = [];
        for (const row of result.results || []) {
          try { rows.push(JSON.parse(row.data)); } catch { /* corrupt row: skip */ }
        }
        mergeFetched(table, rows);
        loadedRows.add(id);
      });
    } catch (error) {
      console.warn(`[DB] row load failed for ${table}/${rowKey}: ${error.message}`);
    }
  }

  // usageHistory on Cloudflare is the budget ledger (plus per-request rows only when low-write mode
  // is explicitly off). A budget read used to load the WHOLE table, every key and every window, into
  // the isolate heap and read it from D1 on each cold start; it grows with every debit, forever.
  // Budget enforcement only needs one key's rows inside its current window, so that is all that is
  // loaded. Two partial expression indexes make those selects read only the matching rows (D1 bills
  // rows scanned, not rows returned); they are created lazily, once, the first time a budget key is read.
  // Two index-driven selects (a single OR makes SQLite scan the whole table instead).
  const HISTORY_SCOPE_SQL =
    `SELECT table_name, data FROM ${D1_TABLE} WHERE table_name = 'usageHistory' ` +
    `AND json_extract(data, '$.apiKeyId') = ? AND json_extract(data, '$.timestamp') >= ? ` +
    `UNION SELECT table_name, data FROM ${D1_TABLE} WHERE table_name = 'usageHistory' ` +
    `AND json_extract(data, '$.apiKey') = ? AND json_extract(data, '$.timestamp') >= ?`;
  const HISTORY_COMMIT_SQL =
    `SELECT table_name, data FROM ${D1_TABLE} WHERE table_name = 'usageHistory' ` +
    `AND json_extract(data, '$.apiKeyId') = ? AND json_extract(data, '$.model') = ?`;
  const HISTORY_INDEX_SQL = [
    `CREATE INDEX IF NOT EXISTS idx_rows_usage_key_id_ts ON ${D1_TABLE}(json_extract(data, '$.apiKeyId'), json_extract(data, '$.timestamp')) WHERE table_name = 'usageHistory'`,
    `CREATE INDEX IF NOT EXISTS idx_rows_usage_key_ts ON ${D1_TABLE}(json_extract(data, '$.apiKey'), json_extract(data, '$.timestamp')) WHERE table_name = 'usageHistory'`,
  ];
  const loadedHistoryScopes = new Set();

  function mergeHistoryRows(rows) {
    for (const row of rows) {
      const parsed = (() => { try { return JSON.parse(row.data); } catch { return null; } })();
      if (!parsed) continue;
      mergeFetched("usageHistory", [parsed]);
    }
  }

  /**
   * Give new usageHistory ids a base no other isolate is likely to share. Ids used to continue from the
   * largest id in a fully loaded table; with the table only partly resident that maximum is unknown, and
   * reusing a persisted id would overwrite another row of the ledger (the row key is the id). A
   * millisecond clock plus a random offset puts every isolate's range far from the others; ids stay
   * monotonic per isolate and above every id minted before.
   */
  function seedHistorySequence() {
    if (loadedTables.has("usageHistory") || state.sequences.usageHistory >= HISTORY_SEQUENCE_FLOOR) return;
    const base = Date.now() * 1000 + Math.floor(Math.random() * 1_000_000);
    state.sequences.usageHistory = Math.max(state.sequences.usageHistory || 0, base);
  }
  const HISTORY_SEQUENCE_FLOOR = 1e15;

  /**
   * Make one key's in-window ledger rows resident (a no-op once the whole table is).
   * A scope counts as loaded only after its rows are merged; a concurrent request therefore never
   * enforces a budget against rows that are still on their way (it used to be marked before the await).
   */
  let historyIndexesDone = false;
  async function ensureBudgetHistory({ apiKeyId, apiKey, since }) {
    seedHistorySequence();
    if (!d1 || loadedTables.has("usageHistory") || ops?.inTransaction()) return;
    const scope = `${apiKeyId || ""}\u0000${apiKey || ""}\u0000${since}`;
    if (loadedHistoryScopes.has(scope)) return;
    // budget enforcement fails closed: a failed or timed-out load rejects, the caller must not assume zero spend
    await loadFlights.run(`history:${scope}`, async () => {
      if (!historyIndexesDone) {
        try {
          await d1.batch(HISTORY_INDEX_SQL.map((sql) => d1.prepare(sql)));
          historyIndexesDone = true;
        } catch (error) {
          console.warn(`[DB] usageHistory index not created: ${error.message}`);
        }
      }
      const result = await d1.prepare(HISTORY_SCOPE_SQL).bind(apiKeyId || "", since, apiKey || "", since).all();
      mergeHistoryRows(result.results || []);
      loadedHistoryScopes.add(scope);
    });
  }

  /** Make a key's ledger row for one commit id resident, whatever window it fell in (idempotent commits). */
  const loadedCommits = new Set();
  async function ensureBudgetCommit(apiKeyId, model) {
    if (!d1 || loadedTables.has("usageHistory") || ops?.inTransaction()) return;
    const id = `${apiKeyId}\u0000${model}`;
    if (loadedCommits.has(id)) return;
    await loadFlights.run(`commit:${id}`, async () => {
      const result = await d1.prepare(HISTORY_COMMIT_SQL).bind(apiKeyId, model).all();
      mergeHistoryRows(result.results || []);
      loadedCommits.add(id);
    });
  }

  // `kv` holds reference data in independent scopes (modelAliases,
  // customModels, pricing, disabledModels, ...). customModels alone can be
  // thousands of rows, and a proxied request almost never needs it, so the
  // table is hydrated per scope: a request reads only the scopes it touches.
  // ensureTables(["kv"]) still loads everything (export/import need that).
  const loadedKvScopes = new Set(); // scopes fully resident
  const KV_SCOPE_SELECT = `SELECT table_name, data FROM ${D1_TABLE} WHERE table_name = 'kv' AND row_key >= ? AND row_key < ?`;

  async function ensureKvScope(scope) {
    if (!d1 || !scope || loadedTables.has("kv") || ops?.inTransaction()) return;
    if (loadedKvScopes.has(scope)) return;
    try {
      await loadFlights.run(`kv:${scope}`, async () => {
        // Row keys are `scope:<scope>|key:<key>`; '|' (0x7C) + 1 = '}' bounds the prefix.
        const result = await d1.prepare(KV_SCOPE_SELECT).bind(`scope:${scope}|`, `scope:${scope}}`).all();
        const rows = [];
        for (const row of result.results || []) {
          try { rows.push(JSON.parse(row.data)); } catch { /* corrupt row: skip, adapter is fail-open */ }
        }
        mergeFetched("kv", rows);
        loadedKvScopes.add(scope);
        fetchedAt.set(`kv:${scope}`, Date.now());
      });
    } catch (error) {
      console.warn(`[DB] kv scope load failed for ${scope}: ${error.message}`);
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
    const stamp = Date.now();
    for (const table of tableNames) fetchedAt.set(table, stamp);
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

  const CREATE_TABLE_SQL =
    `CREATE TABLE IF NOT EXISTS ${D1_TABLE} (` +
    `table_name TEXT NOT NULL, ` +
    `row_key TEXT NOT NULL, ` +
    `data TEXT NOT NULL, ` +
    `updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, ` +
    `PRIMARY KEY (table_name, row_key))`;

  async function initializeD1() {
    if (!d1) return;
    // Read first, create only when the table is missing: the DDL ran on every
    // cold isolate and is a wasted statement everywhere but the very first boot.
    let fresh;
    try {
      fresh = await fetchTablesFromD1(EAGER_TABLES);
    } catch (error) {
      if (!/no such table/i.test(String(error?.message || error))) throw error;
      await d1.prepare(CREATE_TABLE_SQL).run();
      fresh = await fetchTablesFromD1(EAGER_TABLES);
    }
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
    })().catch((error) => noteWriteFailure(`D1 persist failed for ${table}`, error));
    keepAlive(task);
  }

  function persistRow(table, row) {
    if (!d1 || !state.tables[table]) return;
    if (ops?.markDirty(table, row, "upsert")) return;
    const task = d1.prepare(
      `INSERT INTO ${D1_TABLE}(table_name, row_key, data, updated_at) ` +
      `VALUES(?, ?, ?, CURRENT_TIMESTAMP) ` +
      `ON CONFLICT(table_name, row_key) DO UPDATE SET data = excluded.data, updated_at = excluded.updated_at`
    ).bind(table, primaryKeyFor(table, row), JSON.stringify(row)).run()
      .catch((error) => noteWriteFailure(`D1 row persist failed for ${table}`, error));
    keepAlive(task);
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
    )).catch((error) => noteWriteFailure(`D1 row delete failed for ${table}`, error));
    keepAlive(task);
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
      // New rows first, deletions last: a failure or a cancelled task between the two steps then
      // leaves old rows behind instead of a table whose old rows are gone and new rows never arrived.
      // The two key sets are disjoint (one entry per row key), so the order does not change the result.
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
      for (let i = 0; i < deletes.length; i += chunkSize * 4) {
        await d1.batch(deletes.slice(i, i + chunkSize * 4).map((key) =>
          d1.prepare(`DELETE FROM ${D1_TABLE} WHERE table_name = ? AND row_key = ?`).bind(table, key)
        ));
      }
    })().catch((error) => noteWriteFailure(`D1 persist failed for ${table}`, error));
    keepAlive(task);
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
    ensureKvScope,
    ensureBudgetHistory,
    ensureBudgetCommit,
    seedHistorySequence,
    ensureRow,
    freshWithin,
    /**
     * Wait until every D1 write started so far has finished. Rejects when one of them failed, so a
     * caller that must not report success early (a database restore) can tell the user.
     */
    flush: async () => {
      while (pendingWrites.size) await Promise.allSettled([...pendingWrites]);
      // Failures since the previous flush(), including a task that failed before this call.
      if (failedWrites > reportedFailures) {
        reportedFailures = failedWrites;
        throw new Error(`D1 write did not complete (${lastWriteFailure})`);
      }
    },
    close: () => {},
    raw: state,
  };
}
