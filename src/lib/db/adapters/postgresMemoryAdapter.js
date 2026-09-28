// Postgres-backed adapter.
//
// The adapter interface is synchronous (db.run/get/all/transaction across the
// codebase) and Postgres is network I/O, so this cannot be a thin driver wrapper.
// It uses the same shape as cloudflareMemoryAdapter: the working set lives in
// memory, the shared engine in memoryEngine.js interprets SQL against it, and
// writes are persisted asynchronously to Postgres.
//
// Why that is acceptable here, and what it costs: a write is applied to memory
// and returned to the caller synchronously, then persisted. A crash can lose
// whatever is still in the flush queue. So the queue is not uniformly batched —
// tables holding credentials and configuration flush immediately, and only the
// high-volume analytics tables are batched. Losing an apiKey or a provider
// connection to a 250ms timer would be a real bug; losing a usageHistory row
// that is about to be aggregated anyway is not.

import {
  createState,
  createInMemoryOps,
  primaryKeyFor,
  ALL_TABLES,
} from "./memoryEngine.js";
import { buildPostgresSchemaStatements, pgIdent, TABLES } from "./postgresSchema.js";

/**
 * Tables that must not sit in a write-behind queue: anything whose loss is a
 * security or availability incident rather than a rounding error. Provider
 * connections and API keys hold credentials; settings hold the configuration.
 */
const CRITICAL_TABLES = new Set([
  "settings",
  "apiKeys",
  "providerConnections",
  "providerNodes",
  "proxyPools",
  "combos",
  "_meta",
  "kv",
]);

/** Loaded on first touch, not at boot — same reasoning as the D1 adapter. */
const LAZY_TABLES = new Set(["kv", "providerConnections", "usageHistory", "requestDetails"]);

const FLUSH_INTERVAL_MS = 250;
const MAX_QUEUE_BEFORE_FLUSH = 200;

/** Column names per table, so a row can be mapped to a statement safely. */
const COLUMNS = Object.fromEntries(
  Object.entries(TABLES).map(([name, def]) => [name, Object.keys(def.columns)])
);

export async function createPostgresMemoryAdapter(options = {}) {
  const connectionString =
    options.connectionString || process.env.DATABASE_URL || process.env.POSTGRES_URL;
  if (!connectionString) {
    throw new Error("createPostgresMemoryAdapter: DATABASE_URL is required");
  }

  const { Pool } = await import("pg");
  // SSL is left to the connection string and PGSSLMODE rather than forced.
  // Managed providers (Neon, Supabase) put ?sslmode=require in the URL, and pg
  // honours it; a local or in-cluster Postgres has no SSL and fails the handshake
  // if we insist. DB_SSL=true forces it for a TLS-terminating proxy that does
  // not advertise sslmode in its URL.
  const forceSsl = process.env.DB_SSL === "true";
  const pool = new Pool({
    connectionString,
    max: Number(process.env.DB_POOL_MAX || 5),
    idleTimeoutMillis: 30_000,
    connectionTimeoutMillis: 10_000,
    ...(forceSsl ? { ssl: { rejectUnauthorized: false } } : {}),
  });

  const state = createState();
  const loadedTables = new Set(ALL_TABLES.filter((t) => !LAZY_TABLES.has(t)));

  // ── write-behind queue ────────────────────────────────────────────────────
  // Keyed by row identity so a row written several times before a flush costs
  // one statement, not one per write.
  const queue = new Map(); // `${table}|${rowKey}` -> { op, table, row }
  // Flushes are serialised through a chain rather than guarded by a single
  // in-flight promise. Returning the in-flight promise made a concurrent flush()
  // a no-op, so rows queued while that flush was running were simply never
  // written: the caller awaited a promise that had already drained the queue
  // before those rows arrived. Chaining means every flush() drains the queue as
  // of its own call.
  let chain = Promise.resolve();
  let flushTimer = null;
  let closed = false;

  const enqueue = (op, table, row) => {
    if (closed) return;
    queue.set(`${table}|${primaryKeyFor(table, row)}`, { op, table, row });
  };

  function scheduleFlush() {
    if (flushTimer || closed) return;
    flushTimer = setTimeout(() => {
      flushTimer = null;
      void flush();
    }, FLUSH_INTERVAL_MS);
    // Never hold the process open for a pending write.
    flushTimer.unref?.();
  }

  /**
   * Persist everything queued. Runs the whole batch in one transaction so a
   * partially-flushed queue cannot leave the store inconsistent.
   */
  function flush() {
    chain = chain.then(drainOnce, drainOnce);
    return chain;
  }

  async function drainOnce() {
    if (!queue.size) return null;
    const batch = [...queue.values()];
    queue.clear();

    return (async () => {
      const client = await pool.connect();
      try {
        await client.query("BEGIN");
        for (const item of batch) {
          if (item.op === "delete") {
            await client.query(
              deleteSql(item.table, item.row),
              conflictColumns(item.table).map((c) => item.row[c] ?? null)
            );
          } else {
            const values = valuesFor(item.table, item.row);
            if (!values.length) continue;
            await client.query(upsertSql(item.table, item.row), values);
          }
        }
        await client.query("COMMIT");
      } catch (error) {
        await client.query("ROLLBACK").catch(() => {});
        // Requeue so a transient database blip does not silently drop writes.
        for (const item of batch) {
          queue.set(`${item.table}|${primaryKeyFor(item.table, item.row)}`, item);
        }
        // Requeueing a batch that can never succeed (a NOT NULL violation, say)
        // would retry forever and grow unbounded, so only transient failures are
        // retried. A constraint violation is deterministic: it is dropped and
        // reported loudly rather than looping.
        const deterministic = /violates|not-null|null value|duplicate key|invalid input/i.test(
          error.message
        );
        if (deterministic) {
          console.error(`[DB] Postgres rejected ${batch.length} row(s), dropped: ${error.message}`);
        } else {
          for (const item of batch) {
            queue.set(`${item.table}|${primaryKeyFor(item.table, item.row)}`, item);
          }
          console.warn(`[DB] Postgres flush failed, requeued ${batch.length}: ${error.message}`);
        }
      } finally {
        client.release();
      }
    })();
  }

  function upsertSql(table, row) {
    const allowed = COLUMNS[table] || [];
    const columns = allowed.filter((c) => row[c] !== undefined);
    if (!columns.length) return "";
    const placeholders = columns.map((_, i) => `$${i + 1}`);
    const conflictCols = conflictColumns(table);
    const updates = columns
      .filter((c) => !conflictCols.includes(c))
      .map((c) => `${pgIdent(c)} = EXCLUDED.${pgIdent(c)}`);
    const target = `ON CONFLICT (${conflictCols.map(pgIdent).join(", ")})`;
    // When every supplied column is part of the conflict target there is
    // nothing to update, so the statement is a no-op. ON CONFLICT is required
    // here: a bare "DO NOTHING" is a syntax error.
    const verb = updates.length
      ? `${target} DO UPDATE SET ${updates.join(", ")}`
      : `${target} DO NOTHING`;
    return (
      `INSERT INTO ${pgIdent(table)} (${columns.map(pgIdent).join(", ")}) ` +
      `VALUES (${placeholders.join(", ")}) ${verb}`
    );
  }

  function deleteSql(table, row) {
    const conflictCols = conflictColumns(table);
    const where = conflictCols.map((c, i) => `${pgIdent(c)} = $${i + 1}`).join(" AND ");
    return `DELETE FROM ${pgIdent(table)} WHERE ${where}`;
  }

  /**
   * Primary key columns for a table, from the shared schema definition.
   *
   * "PRIMARY KEY (scope, key)" is a composite key and has to be parsed as a
   * column list — a regex written for a single column matched nothing here and
   * left kv with an empty conflict target. A table with no explicit clause
   * uses its first declared column, which is how the in-memory engine and the
   * D1 row key already identify it.
   */
  function conflictColumns(table) {
    const def = TABLES[table];
    if (def?.primaryKey) {
      const list = def.primaryKey.match(/\(([^)]*)\)/);
      const columns = list
        ? list[1].split(",").map((c) => c.trim()).filter(Boolean)
        : [];
      if (columns.length) return columns;
    }
    const first = Object.keys(def?.columns || {})[0];
    return first ? [first] : [];
  }

  function valuesFor(table, row) {
    return (COLUMNS[table] || []).filter((c) => row[c] !== undefined).map((c) => row[c]);
  }

  // ── loading ──────────────────────────────────────────────────────────────

  async function loadTable(table) {
    const columns = COLUMNS[table] || [];
    if (!columns.length) return [];
    const { rows } = await pool.query(
      `SELECT ${columns.map(pgIdent).join(", ")} FROM ${pgIdent(table)}`
    );
    return rows.map(normalizeRow);
  }

  /**
   * Postgres returns BIGINT/NUMERIC as strings to avoid precision loss, and
   * DOUBLE PRECISION as a number. The engine compares with ===, so a numeric
   * that came back as a string would never match. Normalise the few types that
   * do not survive the round trip.
   */
  function normalizeRow(row) {
    const out = {};
    for (const [key, value] of Object.entries(row)) {
      out[key] = value === null || value === undefined ? null : value;
    }
    return out;
  }

  async function bootstrapSchema() {
    const client = await pool.connect();
    try {
      for (const statement of buildPostgresSchemaStatements()) {
        await client.query(statement);
      }
    } finally {
      client.release();
    }
  }

  async function ensureTables(names) {
    const wanted = (Array.isArray(names) ? names : [names]).filter(
      (n) => LAZY_TABLES.has(n) && !loadedTables.has(n)
    );
    if (!wanted.length) return;
    for (const table of wanted) loadedTables.add(table);
    try {
      for (const table of wanted) {
        const rows = await loadTable(table);
        if (!state.tables[table]) state.tables[table] = [];
        // Merge, not append: a row written before this first read is already in
        // memory, and the database has it too. Appending would duplicate it.
        const seen = new Set(state.tables[table].map((r) => primaryKeyFor(table, r)));
        for (const row of rows) {
          const key = primaryKeyFor(table, row);
          if (seen.has(key)) {
            const at = state.tables[table].findIndex((r) => primaryKeyFor(table, r) === key);
            state.tables[table][at] = { ...row, ...state.tables[table][at] };
            continue;
          }
          seen.add(key);
          state.tables[table].push(row);
        }
      }
    } catch (error) {
      // A failed load must not be cached as loaded, or the table stays empty
      // for the life of the process.
      for (const table of wanted) loadedTables.delete(table);
      console.warn(`[DB] Postgres lazy load failed for ${wanted.join(",")}: ${error.message}`);
    }
  }

  // ── persistence hooks ────────────────────────────────────────────────────

  function persistRow(table, row) {
    // Deliberately NOT gated on loadedTables. Lazy loading is a read-side
    // concern; gating writes on it silently dropped every write to kv and
    // providerConnections, which are exactly the two tables most likely to be
    // written before anything has read them.
    enqueue("upsert", table, row);
    if (CRITICAL_TABLES.has(table) || queue.size >= MAX_QUEUE_BEFORE_FLUSH) {
      void flush();
    } else {
      scheduleFlush();
    }
  }

  function deletePersistedRows(table, rows) {
    for (const row of rows) enqueue("delete", table, row);
    if (CRITICAL_TABLES.has(table) || queue.size >= MAX_QUEUE_BEFORE_FLUSH) {
      void flush();
    } else {
      scheduleFlush();
    }
  }

  /** Re-read a table from Postgres, keeping uncommitted local rows. */
  async function resync(force = false, scoped) {
    const wanted = scoped || [...loadedTables];
    for (const table of wanted) {
      // Never clobber a table with uncommitted writes in flight.
      if (ops.hasDirty(table)) continue;
      try {
        const rows = await loadTable(table);
        state.tables[table] = ops.rowsFor(table).filter((local) => {
          const key = primaryKeyFor(table, local);
          return !rows.some((r) => primaryKeyFor(table, r) === key);
        });
        state.tables[table].push(...rows);
      } catch (error) {
        console.warn(`[DB] Postgres resync failed for ${table}: ${error.message}`);
      }
      if (!force) break;
    }
  }

  // ── lifecycle ────────────────────────────────────────────────────────────

  async function close() {
    if (closed) return;
    closed = true;
    if (flushTimer) clearTimeout(flushTimer);
    await flush();
    await pool.end();
  }

  const ops = createInMemoryOps(state, {
    persistRow,
    deletePersistedRows,
    persistDirtyRows(table) {
      const touched = ops.pendingDirtyRows().get(table);
      ops.pendingDirtyRows().delete(table);
      if (!touched || !touched.size) return;
      for (const [key, op] of touched) {
        if (op === "delete") {
          enqueue("delete", table, { ...keyToRow(table, key) });
          continue;
        }
        const row = ops.rowsFor(table).find((r) => primaryKeyFor(table, r) === key);
        if (row) enqueue("upsert", table, row);
        else enqueue("delete", table, keyToRow(table, key));
      }
      if (CRITICAL_TABLES.has(table) || queue.size >= MAX_QUEUE_BEFORE_FLUSH) {
        void flush();
      } else {
        scheduleFlush();
      }
    },
  });

  /** Rebuild a minimal row from a composite key string, for a deferred delete. */
  function keyToRow(table, key) {
    const keys = (TABLES[table] && conflictColumns(table)) || [];
    const row = {};
    keys.forEach((column, index) => {
      const prefix = `${column}:`;
      const start = key.indexOf(prefix);
      if (start < 0) return;
      const from = start + prefix.length;
      const end = index + 1 < keys.length ? key.indexOf("|", from) : key.length;
      row[column] = key.slice(from, end < 0 ? key.length : end);
    });
    return row;
  }

  await bootstrapSchema();
  for (const table of loadedTables) {
    state.tables[table] = await loadTable(table);
  }

  // Drain on the way out. Without this a deploy or a restart loses the queue.
  const onExit = () => {
    if (closed || !queue.size) return;
    pool.end().catch(() => {});
  };
  process.once("beforeExit", onExit);
  for (const signal of ["SIGTERM", "SIGINT"]) {
    process.once(signal, () => {
      flush().finally(() => pool.end().catch(() => {}));
    });
  }

  return {
    driver: "postgres-backed-memory",
    run: ops.run,
    get: ops.get,
    all: ops.all,
    exec: ops.exec,
    transaction: ops.transaction,
    resync,
    ensureTables,
    flush,
    close,
    raw: state,
  };
}
