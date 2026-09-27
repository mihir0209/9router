import { TABLES } from "../schema.js";

const D1_TABLE = "ninerouter_rows";

const PRIMARY_KEYS = {
  _meta: ["key"],
  settings: ["id"],
  providerConnections: ["id"],
  providerNodes: ["id"],
  proxyPools: ["id"],
  apiKeys: ["id"],
  combos: ["id"],
  kv: ["scope", "key"],
  usageHistory: ["id"],
  usageDaily: ["dateKey"],
  requestDetails: ["id"],
  auditLog: ["id"],
};

const AUTOINCREMENT = new Set(["usageHistory"]);

async function getD1Context() {
  try {
    const { getCloudflareContext } = await import("@opennextjs/cloudflare");
    return await getCloudflareContext({ async: true });
  } catch (error) {
    console.warn(`[DB] Cloudflare D1 context unavailable: ${error.message}`);
    return null;
  }
}

function primaryKeyFor(table, row) {
  const keys = PRIMARY_KEYS[table] || [];
  if (!keys.length) return row.id ?? JSON.stringify(row);
  return keys.map((key) => `${key}:${String(row[key] ?? "")}`).join("|");
}

function clone(row) {
  return row ? { ...row } : row;
}

function normalizeSql(sql) {
  return String(sql || "").replace(/\s+/g, " ").trim();
}

function splitCsv(input) {
  const out = [];
  let current = "";
  let quote = null;
  let depth = 0;
  for (const ch of input) {
    if (quote) {
      current += ch;
      if (ch === quote) quote = null;
      continue;
    }
    if (ch === "'" || ch === '"') {
      quote = ch;
      current += ch;
      continue;
    }
    if (ch === "(") depth++;
    if (ch === ")") depth--;
    if (ch === "," && depth === 0) {
      out.push(current.trim());
      current = "";
      continue;
    }
    current += ch;
  }
  if (current.trim()) out.push(current.trim());
  return out;
}

function literalValue(token, params, cursor) {
  const t = token.trim();
  if (t === "?") return { value: params[cursor.value++], cursor };
  if (/^null$/i.test(t)) return { value: null, cursor };
  if (/^true$/i.test(t)) return { value: true, cursor };
  if (/^false$/i.test(t)) return { value: false, cursor };
  if ((t.startsWith("'") && t.endsWith("'")) || (t.startsWith('"') && t.endsWith('"'))) {
    return { value: t.slice(1, -1), cursor };
  }
  if (/^-?\d+(\.\d+)?$/.test(t)) return { value: Number(t), cursor };
  return { value: t, cursor };
}

function compare(a, op, b) {
  if (op === "=") return a === b;
  if (op === "!=" || op === "<>") return a !== b;
  if (op === ">=") return a >= b;
  if (op === "<=") return a <= b;
  if (op === ">") return a > b;
  if (op === "<") return a < b;
  return false;
}

function buildWhere(whereSql, params) {
  if (!whereSql) return () => true;
  const parts = whereSql.split(/\s+AND\s+/i).map((p) => p.trim()).filter(Boolean);
  const cursor = { value: 0 };
  const tests = parts.map((part) => {
    let m = part.match(/^(\w+)\s+IS\s+NOT\s+NULL$/i);
    if (m) return (row) => row[m[1]] !== null && row[m[1]] !== undefined;

    m = part.match(/^(\w+)\s+IS\s+NULL$/i);
    if (m) return (row) => row[m[1]] === null || row[m[1]] === undefined;

    m = part.match(/^(\w+)\s+IN\s*\((.+)\)$/i);
    if (m) {
      const values = splitCsv(m[2]).map((token) => literalValue(token, params, cursor).value);
      return (row) => values.includes(row[m[1]]);
    }

    m = part.match(/^(\w+)\s*(=|!=|<>|>=|<=|>|<)\s*(.+)$/i);
    if (m) {
      const [, field, op, rhs] = m;
      const { value } = literalValue(rhs, params, cursor);
      return (row) => compare(row[field], op, value);
    }

    return () => true;
  });
  return (row) => tests.every((test) => test(row));
}

function parseSelect(sql) {
  const match = sql.match(/^SELECT\s+(.+?)\s+FROM\s+(\w+)(?:\s+WHERE\s+(.+?))?(?:\s+ORDER\s+BY\s+(.+?))?(?:\s+LIMIT\s+(.+?))?$/i);
  if (!match) return null;
  return {
    columns: match[1].trim(),
    table: match[2],
    where: match[3]?.trim(),
    order: match[4]?.trim(),
    limit: match[5]?.trim(),
  };
}

function projectRows(rows, columns) {
  if (columns === "*") return rows.map(clone);

  const coalesceSum = columns.match(/^COALESCE\(\s*SUM\(([^)]+)\)\s*,\s*0\s*\)\s*(?:AS\s+)?(\w+)$/i);
  if (coalesceSum) {
    const expression = coalesceSum[1].split("+").map((part) => part.trim());
    return [{ [coalesceSum[2]]: rows.reduce((total, row) => total + expression.reduce((sum, field) => sum + (Number(row[field]) || 0), 0), 0) }];
  }
  const sum = columns.match(/^SUM\(([^)]+)\)\s*(?:AS\s+)?(\w+)$/i);
  if (sum) return [{ [sum[2]]: rows.reduce((total, row) => total + (Number(row[sum[1].trim()]) || 0), 0) }];

  const count = columns.match(/^COUNT\(\*\)\s+(?:AS\s+)?(\w+)$/i);
  if (count) return [{ [count[1]]: rows.length }];

  const distinct = columns.match(/^DISTINCT\s+(\w+)$/i);
  if (distinct) {
    const key = distinct[1];
    return [...new Set(rows.map((row) => row[key]).filter((value) => value !== null && value !== undefined))]
      .map((value) => ({ [key]: value }));
  }

  if (columns === "1") return rows.map(() => ({ 1: 1 }));

  const fields = splitCsv(columns).map((field) => field.trim());
  return rows.map((row) => {
    const out = {};
    for (const field of fields) {
      const m = field.match(/^(\w+)(?:\s+AS\s+(\w+))?$/i);
      if (!m) continue;
      out[m[2] || m[1]] = row[m[1]];
    }
    return out;
  });
}

function createState() {
  const tables = {};
  for (const table of Object.keys(TABLES)) tables[table] = [];
  return { tables, sequences: {} };
}

export async function createCloudflareMemoryAdapter() {
  const state = createState();
  const context = await getD1Context();
  const d1 = context?.env?.DB || null;
  let transactionDepth = 0;
  const dirtyTables = new Set();
  // Row-level dirty tracking inside transactions: table -> Map(rowKey, "upsert"|"delete").
  // Commit persists ONLY touched rows. Previously every transaction rewrote each
  // touched table wholesale (DELETE all + re-INSERT all), so a kv model sync
  // cost ~7000 D1 row writes and a requestDetails save ~700. On metered D1
  // (100k writes/day) that amplification is the budget killer.
  const dirtyRows = new Map();

  // Tables whose row count is bounded by configuration rather than by traffic.
  // usageHistory and requestDetails grow by one persisted row per request, so
  // eagerly loading them meant every cold isolate read and JSON.parsed the entire
  // request history before serving anything — which exhausted the Worker's
  // resource limits (CF 1102) and made the instance fail most requests. They are
  // analytics: nothing on the request path needs them resident, and an explicit
  // resync(force, ["usageHistory"]) can still pull them on demand.
  const EAGER_TABLES = Object.keys(TABLES).filter(
    (table) => table !== "usageHistory" && table !== "requestDetails"
  );

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
    const wanted = scoped || EAGER_TABLES;
    // Snapshot each table's row count. Anything awaited below yields to other
    // requests, and one of them may write to a table we are about to replace.
    // A table whose length moved while we were fetching has been written to and
    // must not be overwritten. Clearing state up front instead would silently
    // drop writes that only ever existed in memory.
    const before = new Map(wanted.map((table) => [table, (state.tables[table] || []).length]));
    try {
      const fresh = await fetchTablesFromD1(wanted);
      for (const table of wanted) {
        if (dirtyTables.has(table) || transactionDepth > 0) continue;
        // Rows are pushed in place, so the array identity is unchanged by a
        // concurrent write; its length is what moves.
        if ((state.tables[table] || []).length !== before.get(table)) continue; // written to mid-fetch
        state.tables[table] = fresh[table] || [];
      }
    } catch (error) {
      console.warn(`[DB] D1 resync failed: ${error.message}`);
    }
  }

  // Last-op-wins per row key: delete-then-insert resolves to upsert (row is
  // present at commit), insert-then-delete resolves to delete (row is gone).
  function markDirty(table, row = null, op = "upsert") {
    if (transactionDepth <= 0) return false;
    dirtyTables.add(table);
    if (row) {
      if (!dirtyRows.has(table)) dirtyRows.set(table, new Map());
      dirtyRows.get(table).set(primaryKeyFor(table, row), op);
    }
    return true;
  }

  function persistTable(table) {
    if (!d1 || !state.tables[table]) return;
    if (transactionDepth > 0) {
      dirtyTables.add(table);
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
    if (markDirty(table, row, "upsert")) return;
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
    if (transactionDepth > 0) {
      for (const row of rows) markDirty(table, row, "delete");
      return;
    }
    const task = d1.batch(rows.map((row) =>
      d1.prepare(`DELETE FROM ${D1_TABLE} WHERE table_name = ? AND row_key = ?`)
        .bind(table, primaryKeyFor(table, row))
    )).catch((error) => console.warn(`[DB] D1 row delete failed for ${table}: ${error.message}`));
    context?.ctx?.waitUntil?.(task);
  }

  await initializeD1();

  function rowsFor(table) {
    if (!state.tables[table]) state.tables[table] = [];
    return state.tables[table];
  }

  function findConflictIndex(table, row, conflictCols) {
    const keys = conflictCols?.length ? conflictCols : PRIMARY_KEYS[table] || [];
    if (!keys.length) return -1;
    return rowsFor(table).findIndex((existing) => keys.every((key) => existing[key] === row[key]));
  }

  function runInsert(sql, params) {
    const match = sql.match(/^INSERT(?:\s+OR\s+REPLACE)?\s+INTO\s+(\w+)\s*\((.+?)\)\s*VALUES\s*\((.+?)\)(?:\s+ON\s+CONFLICT\s*\((.+?)\)\s+DO\s+UPDATE\s+SET\s+(.+))?$/i);
    if (!match) return { changes: 0, lastInsertRowid: null };

    const [, table, columnsSql, valuesSql, conflictSql] = match;
    const columns = splitCsv(columnsSql);
    const values = splitCsv(valuesSql);
    const cursor = { value: 0 };
    const row = {};
    columns.forEach((column, index) => {
      row[column] = literalValue(values[index], params, cursor).value;
    });

    if (AUTOINCREMENT.has(table) && (row.id === undefined || row.id === null)) {
      state.sequences[table] = (state.sequences[table] || 0) + 1;
      row.id = state.sequences[table];
    }

    const conflictCols = conflictSql ? splitCsv(conflictSql) : PRIMARY_KEYS[table];
    const index = findConflictIndex(table, row, conflictCols);
    const replace = /^INSERT\s+OR\s+REPLACE/i.test(sql);
    if (index >= 0) {
      rowsFor(table)[index] = replace ? row : { ...rowsFor(table)[index], ...row };
      persistRow(table, rowsFor(table)[index]);
    } else {
      rowsFor(table).push(row);
      persistRow(table, row);
    }
    return { changes: 1, lastInsertRowid: row.id ?? null };
  }

  function runUpdate(sql, params) {
    const match = sql.match(/^UPDATE\s+(\w+)\s+SET\s+(.+?)\s+WHERE\s+(.+)$/i);
    if (!match) return { changes: 0, lastInsertRowid: null };
    const [, table, setSql, whereSql] = match;
    const assignments = splitCsv(setSql);
    const setCursor = { value: 0 };
    const updates = {};
    for (const assignment of assignments) {
      const m = assignment.match(/^(\w+)\s*=\s*(.+)$/i);
      if (!m) continue;
      updates[m[1]] = literalValue(m[2], params, setCursor).value;
    }
    const whereParams = params.slice(setCursor.value);
    const where = buildWhere(whereSql, whereParams);
    let changes = 0;
    const changedRows = [];
    for (const row of rowsFor(table)) {
      if (!where(row)) continue;
      Object.assign(row, updates);
      changedRows.push(row);
      changes++;
    }
    for (const row of changedRows) persistRow(table, row);
    return { changes, lastInsertRowid: null };
  }

  function runDelete(sql, params) {
    const match = sql.match(/^DELETE\s+FROM\s+(\w+)(?:\s+WHERE\s+(.+))?$/i);
    if (!match) return { changes: 0, lastInsertRowid: null };
    const [, table, whereSql] = match;
    const where = buildWhere(whereSql, params);
    const existingRows = rowsFor(table);
    const deletedRows = existingRows.filter((row) => where(row));
    const before = existingRows.length;
    state.tables[table] = existingRows.filter((row) => !where(row));
    const changes = before - state.tables[table].length;
    if (changes) deletePersistedRows(table, deletedRows);
    return { changes, lastInsertRowid: null };
  }

  function run(sql, params = []) {
    const normalized = normalizeSql(sql);
    if (/^INSERT/i.test(normalized)) return runInsert(normalized, params);
    if (/^UPDATE/i.test(normalized)) return runUpdate(normalized, params);
    if (/^DELETE/i.test(normalized)) return runDelete(normalized, params);
    return { changes: 0, lastInsertRowid: null };
  }

  function all(sql, params = []) {
    const normalized = normalizeSql(sql).replace(/;$/, "");
    const parsed = parseSelect(normalized);
    if (!parsed) return [];
    let rows = rowsFor(parsed.table).filter(buildWhere(parsed.where, params));

    if (parsed.order) {
      const [field, direction = "ASC"] = parsed.order.split(/\s+/);
      rows = [...rows].sort((a, b) => {
        const av = a[field];
        const bv = b[field];
        if (av === bv) return 0;
        const result = av > bv ? 1 : -1;
        return direction.toUpperCase() === "DESC" ? -result : result;
      });
    }

    if (parsed.limit) {
      const cursor = { value: 0 };
      const limit = Number(literalValue(parsed.limit, params, cursor).value);
      rows = rows.slice(0, Number.isFinite(limit) ? limit : undefined);
    }

    return projectRows(rows, parsed.columns);
  }

  function get(sql, params = []) {
    return all(sql, params)[0];
  }

  function exec(sql) {
    for (const statement of String(sql || "").split(";")) {
      const normalized = normalizeSql(statement);
      if (normalized) run(normalized);
    }
    return [];
  }

  function transaction(fn) {
    transactionDepth++;
    try {
      return fn();
    } finally {
      transactionDepth--;
      if (transactionDepth === 0 && dirtyTables.size) {
        const tables = [...dirtyTables];
        dirtyTables.clear();
        for (const table of tables) persistDirtyRows(table);
      }
    }
  }

  // Commit path: persist only rows touched inside the transaction. Tables
  // with no row-level record (defensive; all writers record rows) fall back
  // to the legacy whole-table rewrite.
  function persistDirtyRows(table) {
    if (!d1 || !state.tables[table]) {
      dirtyRows.delete(table);
      return;
    }
    const touched = dirtyRows.get(table);
    dirtyRows.delete(table);
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
      const row = rowsFor(table).find((r) => primaryKeyFor(table, r) === key);
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
    run,
    get,
    all,
    exec,
    transaction,
    resync: resyncFromD1,
    close: () => {},
    raw: state,
  };
}
