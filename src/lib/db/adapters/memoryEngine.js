// Shared in-memory SQL engine.
//
// Both the Cloudflare (D1) adapter and the Postgres adapter serve a
// *synchronous* adapter interface — db.run/get/all/transaction — over
// asynchronous backing stores. SQLite does this natively; neither D1 nor
// Postgres can, because both are network I/O.
//
// The trick used here is the same one cloudflareMemoryAdapter already relies
// on: keep the authoritative working set in memory, interpret the small,
// predictable SQL subset this codebase actually issues against those JS
// objects, and persist asynchronously at a separate boundary. Because both
// adapters run the SAME interpreter over the SAME schema, their semantics
// cannot drift — a row written through one behaves identically through the
// other, which is what makes the Postgres port safe to trust.
//
// This file is deliberately pure: no I/O, no state, no adapter-specific
// knowledge. Persistence lives in the adapters.

import { TABLES } from "../schema.js";

/** Composite primary keys, used for row identity when persisting. */
export const PRIMARY_KEYS = {
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

/** Tables whose integer key is generated in-process rather than by the store. */
export const AUTOINCREMENT = new Set(["usageHistory"]);

export function primaryKeyFor(table, row) {
  const keys = PRIMARY_KEYS[table] || [];
  if (!keys.length) return row.id ?? JSON.stringify(row);
  return keys.map((key) => `${key}:${String(row[key] ?? "")}`).join("|");
}

export function clone(row) {
  return row ? { ...row } : row;
}

export function normalizeSql(sql) {
  return String(sql || "").replace(/\s+/g, " ").trim();
}

export function splitCsv(input) {
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

export function literalValue(token, params, cursor) {
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

export function compare(a, op, b) {
  if (op === "=") return a === b;
  if (op === "!=" || op === "<>") return a !== b;
  if (op === ">=") return a >= b;
  if (op === "<=") return a <= b;
  if (op === ">") return a > b;
  if (op === "<") return a < b;
  return false;
}

export function buildWhere(whereSql, params) {
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

export function parseSelect(sql) {
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

export function projectRows(rows, columns) {
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

export function createState() {
  const tables = {};
  for (const table of Object.keys(TABLES)) tables[table] = [];
  return { tables, sequences: {} };
}

export const ALL_TABLES = Object.keys(TABLES);

/**
 * The in-memory row operations, parameterised by how rows reach durable storage.
 *
 * This is the whole adapter body shared by every async backing store. It owns:
 * interpreting the SQL subset this codebase issues, applying it to JS objects,
 * tracking what a transaction touched, and calling the persistence hooks at the
 * right moment (immediately outside a transaction, batched at commit inside one).
 *
 * A backing store supplies only the three hooks and the row loader. Because the
 * SQL interpretation, the conflict handling, the autoincrement sequence and the
 * transaction semantics are literally the same code for D1 and Postgres, a row
 * written through one adapter behaves identically through the other. That is the
 * property that makes the Postgres port safe: not a second implementation to
 * keep in step by hand, but one implementation over a different socket.
 *
 * @param {object} options
 * @param {object} options.state                 `{ tables, sequences }` from createState()
 * @param {(table: string, row: object) => void} options.persistRow
 * @param {(table: string, rows: object[]) => void} options.deletePersistedRows
 * @param {(table: string) => void} options.persistDirtyRows  commit-time flush
 * @param {(table: string) => object[]} [options.loadRows]    load a table from storage
 */
export function createInMemoryOps(state, {
  persistRow = () => {},
  deletePersistedRows = () => {},
  persistDirtyRows = () => {},
} = {}) {
  let transactionDepth = 0;
  const dirtyTables = new Set();
  const dirtyRows = new Map();

  function rowsFor(table) {
    if (!state.tables[table]) state.tables[table] = [];
    return state.tables[table];
  }

  /**
   * Record a row touched by an open transaction. Returns true when the caller
   * should NOT persist now — inside a transaction the commit does it, so that a
   * multi-row write costs one batch instead of one round trip per row.
   */
  function markDirty(table, row = null, op = "upsert") {
    if (transactionDepth <= 0) return false;
    dirtyTables.add(table);
    // A null row means "this whole table changed" — the backing store falls
    // back to a whole-table rewrite rather than a row diff.
    if (row) {
      if (!dirtyRows.has(table)) dirtyRows.set(table, new Map());
      dirtyRows.get(table).set(primaryKeyFor(table, row), op);
    }
    return true;
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

  return {
    run,
    get,
    all,
    exec,
    transaction,
    rowsFor,
    markDirty,
    inTransaction: () => transactionDepth > 0,
    /** Has this table got uncommitted changes? A resync must not clobber them. */
    hasDirty: (table) => dirtyTables.has(table),
    /** Row keys touched since the last commit, for a backing store to flush. */
    pendingDirtyRows: () => dirtyRows,
  };
}
