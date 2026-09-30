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

/**
 * Evaluate a small SQL expression against a row.
 *
 * The SET clause needs this, not just literals: "regenerateVersion =
 * COALESCE(regenerateVersion, 0) + 1" has to read the row being updated. With
 * literal-only parsing the whole expression was stored as a string, so
 * regenerating an API key on Workers wrote the text
 * "COALESCE(regenerateVersion, 0) + 1" into an INTEGER column.
 *
 * Handles: params, quoted/numeric/null/boolean literals, bare columns,
 * COALESCE(...), parenthesised arithmetic with + - * /, and substr(col, a, b).
 */
export function evalExpression(token, params, cursor, row = {}) {
  const src = String(token || "").trim();
  if (!src) return null;

  // Strip an alias before evaluating.
  const alias = src.split(/\s+AS\s+/i);
  const expr = alias[0].trim();
  const aliasName = alias[1] ? alias[1].trim() : null;

  const value = evaluateNode(expr, params, cursor, row);
  return { value, alias: aliasName };
}

/**
 * Arguments of `NAME(...)` only when the call spans the whole expression.
 *
 * A plain `/^COALESCE\s*\((.+)\)$/` is not enough: the `.+` is greedy, so
 * "COALESCE(a, 0) + COALESCE(b, 0)" matched with the argument list
 * "a, 0) + COALESCE(b, 0" and the first non-null argument won. Every compound
 * aggregate — a token total added from two SUMs — evaluated to only its first
 * term, with nothing to notice it by.
 */
function balancedCallArgs(src, namePattern) {
  const m = src.match(new RegExp(`^(${namePattern})\\s*\\(`, "i"));
  if (!m) return null;
  const open = m[0].length - 1;
  let depth = 0;
  for (let i = open; i < src.length; i++) {
    const ch = src[i];
    if (ch === "(") depth++;
    else if (ch === ")") {
      depth--;
      // The call has to close exactly at the end of the expression.
      if (depth === 0) return i === src.length - 1 ? src.slice(open + 1, i) : null;
    }
  }
  return null;
}

function evaluateNode(src, params, cursor, row) {
  const text = String(src || "").trim();

  // Literals and parameters first — before column lookup, so the string "1" is
  // a number and not a column called 1.
  const lit = literalValue(text, params, { value: cursor.value });
  // literalValue advances its own cursor; keep ours in step.
  if (text === "?" || /^(null|true|false)$/i.test(text) || /^'.*'$/.test(text) || /^".*"$/.test(text) || /^-?\d+(\.\d+)?$/.test(text)) {
    cursor.value = lit.cursor.value;
    return lit.value;
  }

  const coalesceArgs = balancedCallArgs(text, "COALESCE");
  if (coalesceArgs !== null) {
    for (const arg of splitCsv(coalesceArgs)) {
      const v = evaluateNode(arg, params, cursor, row);
      if (v !== null && v !== undefined) return v;
    }
    return null;
  }

  const subArgs = balancedCallArgs(text, "(?:substr|substring)");
  if (subArgs !== null) {
    const args = splitCsv(subArgs);
    const [value, from, len] = args.map((a) => evaluateNode(a, params, cursor, row));
    if (value === null || value === undefined) return null;
    const start = Math.max(0, (Number(from) || 1) - 1);
    const count = len === undefined ? undefined : Number(len);
    return String(value).slice(start, count === undefined ? undefined : start + count);
  }

  const sub = text.match(/^(?:substr|substring)\s*\((.+)\)$/i);
  if (sub) {
    const args = splitCsv(sub[1]);
    const [value, from, len] = args.map((a) => evaluateNode(a, params, cursor, row));
    if (value === null || value === undefined) return null;
    const start = Math.max(0, (Number(from) || 1) - 1);
    const count = len === undefined ? undefined : Number(len);
    return String(value).slice(start, count === undefined ? undefined : start + count);
  }

  // Arithmetic, respecting the innermost group.
  for (const [pattern, apply] of [
    [/^\((.*)\)$/, null],
  ]) {
    if (apply === null) {
      const m = text.match(pattern);
      if (m) return evaluateNode(m[1], params, cursor, row);
    }
  }
  const arith = splitArithmetic(text);
  if (arith) {
    const { parts, ops } = arith;
    let acc = evaluateNode(parts[0], params, cursor, row);
    for (let i = 0; i < ops.length; i++) {
      const rhs = evaluateNode(parts[i + 1], params, cursor, row);
      const a = Number(acc) || 0;
      const b = Number(rhs) || 0;
      if (ops[i] === "+") acc = a + b;
      else if (ops[i] === "-") acc = a - b;
      else if (ops[i] === "*") acc = a * b;
      else if (ops[i] === "/") acc = b === 0 ? null : a / b;
      else acc = rhs;
    }
    return acc;
  }

  if (Object.prototype.hasOwnProperty.call(row, text)) return row[text];
  return text;
}

/** Number of "?" placeholders in a SQL fragment, ignoring those in quotes. */
export function countPlaceholders(sql) {
  let count = 0;
  let quote = null;
  const text = String(sql || "");
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (quote) {
      if (ch === quote) quote = null;
      continue;
    }
    if (ch === "'" || ch === '"') { quote = ch; continue; }
    if (ch === "?") count++;
  }
  return count;
}

/** Split "a + b - c" on top-level arithmetic operators, respecting parens. */
function splitArithmetic(src) {
  const parts = [];
  const ops = [];
  let depth = 0;
  let current = "";
  for (let i = 0; i < src.length; i++) {
    const ch = src[i];
    if (ch === "(") depth++;
    if (ch === ")") depth--;
    if (depth === 0 && (ch === "+" || ch === "-" || ch === "*" || ch === "/") && current.trim() && i > 0) {
      parts.push(current);
      ops.push(ch);
      current = "";
      continue;
    }
    current += ch;
  }
  if (!ops.length) return null;
  parts.push(current);
  return { parts: parts.map((x) => x.trim()), ops };
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

export function buildWhere(whereSql, params, sharedCursor) {
  if (!whereSql) return () => true;
  const parts = whereSql.split(/\s+AND\s+/i).map((p) => p.trim()).filter(Boolean);
  // The cursor is shared with the caller so that placeholders after the WHERE
  // clause — LIMIT ?, OFFSET ? — read the parameters that belong to them. With a
  // private cursor, "WHERE provider = ? LIMIT ?" bound the limit to the provider
  // value instead.
  const cursor = sharedCursor || { value: 0 };
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
  // OFFSET has to be matched explicitly. Left out, the LIMIT group swallowed
  // "LIMIT ? OFFSET ?" whole, Number() of that was NaN, and the query silently
  // returned every row — so a request for page 9999 of the request-details tab
  // answered with the whole table. This is the engine the D1 adapter serves too,
  // so pagination was broken on Workers as well; it only stayed invisible because
  // SQLite runs the real thing.
  //
  // GROUP BY sits before ORDER BY for the same reason: an unrecognised clause
  // made the whole match fail, parseSelect returned null, and `all` returned []
  // with no error. Every aggregate report — usage stats, per-key daily usage —
  // therefore came back empty on the adapters that use this engine (Cloudflare
  // D1 and Postgres), while SQLite ran the real query and looked fine.
  const match = sql.match(
    /^SELECT\s+(.+?)\s+FROM\s+(\w+)(?:\s+WHERE\s+(.+?))?(?:\s+GROUP\s+BY\s+(.+?))?(?:\s+ORDER\s+BY\s+(.+?))?(?:\s+LIMIT\s+([^\s]+))?(?:\s+OFFSET\s+([^\s]+))?$/i
  );
  if (!match) return null;
  return {
    columns: match[1].trim(),
    table: match[2],
    where: match[3]?.trim(),
    groupBy: match[4]?.trim(),
    order: match[5]?.trim(),
    limit: match[6]?.trim(),
    offset: match[7]?.trim(),
  };
}

const AGGREGATE_NAMES = ["COUNT", "SUM", "MIN", "MAX", "AVG"];

/** The column a bare aggregate names its result after, or null if it is not one. */
function bareAggregateColumn(expression) {
  const m = expression.match(
    new RegExp(`^(${AGGREGATE_NAMES.join("|")})\\s*\\(\\s*([\\w.]+)\\s*\\)$`, "i")
  );
  return m ? m[2] : null;
}

/** True when a select item contains an aggregate and so collapses a group to one value. */
function containsAggregate(expression) {
  return new RegExp(`\\b(${AGGREGATE_NAMES.join("|")})\\s*\\(`, "i").test(expression);
}

/** Compute one aggregate over the rows of a group. */
function computeAggregate(fn, argument, rows) {
  const upper = fn.toUpperCase();
  if (upper === "COUNT") {
    if (!argument || argument === "*") return rows.length;
    return rows.filter((row) => row[argument] !== null && row[argument] !== undefined).length;
  }
  if (upper === "SUM") {
    return rows.reduce((total, row) => total + (Number(row[argument]) || 0), 0);
  }
  if (upper === "AVG") {
    const values = rows.map((row) => Number(row[argument])).filter((n) => Number.isFinite(n));
    return values.length ? values.reduce((a, b) => a + b, 0) / values.length : null;
  }
  // MIN/MAX. Empty input yields null rather than 0: MAX of nothing is null, and a
  // 0 here would be indistinguishable from a real minimum.
  const values = rows
    .map((row) => row[argument])
    .filter((value) => value !== null && value !== undefined);
  if (!values.length) return null;
  return values.reduce((acc, v) => (upper === "MAX" ? (v > acc ? v : acc) : v < acc ? v : acc));
}

/**
 * Project one group of rows.
 *
 * Handles the shapes real reporting queries use: a bare column, an aggregate, an
 * aggregate wrapped in COALESCE, a sum of two coalesced aggregates, or a plain
 * expression such as substr(timestamp, 1, 10) used as the group label. A select
 * item that is none of those used to be dropped by the old column-list parser,
 * which returned {} for the row — the caller saw undefined for every field.
 */
function projectGroup(rows, columns, params) {
  const fields = splitCsv(columns);
  const out = {};
  for (const raw of fields) {
    const field = raw.trim();
    if (!field) continue;
    const aliased = field.match(/^(.*?)\s+AS\s+(\w+)$/i);
    const expression = (aliased ? aliased[1] : field).trim();
    const alias = aliased ? aliased[2] : null;

    if (containsAggregate(expression)) {
      // Substitute every aggregate in the item, not just the first: report
      // queries add two of them ("COALESCE(SUM(promptTokens),0) +
      // COALESCE(SUM(completionTokens),0) AS tokens") and leaving the second as
      // literal text silently produced a wrong total.
      let rewritten = expression;
      const pattern = new RegExp(`\\b(${AGGREGATE_NAMES.join("|")})\\s*\\(\\s*([^)]*?)\\s*\\)`, "gi");
      for (let guard = 0; guard < 16; guard++) {
        const inner = pattern.exec(rewritten);
        if (!inner) break;
        const value = computeAggregate(inner[1], inner[2].trim(), rows);
        rewritten = `${rewritten.slice(0, inner.index)}${value}${rewritten.slice(inner.index + inner[0].length)}`;
        pattern.lastIndex = 0;
      }
      const cursor = { value: 0 };
      const { value } = evalExpression(rewritten, params, cursor, rows[0] || {});
      // A bare aggregate is named after its column, not after its own text:
      // "SELECT MAX(priority)" has to come back as { priority: 7 } because that
      // is what the column-list parser used to produce and callers read it.
      const defaultName = bareAggregateColumn(expression) || expression;
      out[alias || defaultName] = value;
      continue;
    }

    // Not an aggregate: a per-row expression, constant across the group.
    const bare = expression.match(/^([\w.]+)$/);
    if (bare) {
      out[alias || bare[1]] = rows[0] ? rows[0][bare[1]] : undefined;
      continue;
    }
    const cursor = { value: 0 };
    const { value } = evalExpression(expression, params, cursor, rows[0] || {});
    out[alias || expression] = value;
  }
  return out;
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

  // MIN/MAX/AVG. These were missing, and a missing aggregate does not throw —
  // it falls through to the column-list branch, which cannot parse the parens,
  // so it returns {}. So "SELECT MAX(priority) AS m" came back as an empty
  // object, `maxRow?.m` was undefined, and provider priority was computed as
  // (0 || 0) + 1 — i.e. every connection got priority 1. Provider ordering was
  // silently broken on the adapters that use this engine (Cloudflare D1), and
  // only visible because the SQLite path runs real SQL.
  //
  // Empty input yields null rather than 0: MAX of nothing is null, and a 0 here
  // would be indistinguishable from a real minimum.
  const extremum = columns.match(/^(MIN|MAX)\(\s*([\w.]+)\s*\)\s*(?:AS\s+)?(\w+)?$/i);
  if (extremum) {
    const [, fn, field, alias] = extremum;
    const values = rows
      .map((row) => row[field])
      .filter((value) => value !== null && value !== undefined);
    const result = values.length
      ? values.reduce((acc, v) => (fn.toUpperCase() === "MAX" ? (v > acc ? v : acc) : v < acc ? v : acc))
      : null;
    return [{ [alias || field]: result }];
  }

  const average = columns.match(/^AVG\(\s*([\w.]+)\s*\)\s*(?:AS\s+)?(\w+)?$/i);
  if (average) {
    const [, field, alias] = average;
    const values = rows
      .map((row) => Number(row[field]))
      .filter((value) => Number.isFinite(value));
    return [{ [alias || field]: values.length ? values.reduce((a, b) => a + b, 0) / values.length : null }];
  }

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
    // WHERE is optional, matching SQL. runDelete already allowed that; runUpdate
    // demanded it, so an unconditional UPDATE silently matched nothing and
    // reported changes: 0 as though it had run.
    const match = sql.match(/^UPDATE\s+(\w+)\s+SET\s+(.+?)(?:\s+WHERE\s+(.+))?$/i);
    if (!match) return { changes: 0, lastInsertRowid: null };
    const [, table, setSql, whereSql] = match;
    const assignments = splitCsv(setSql);
    const updates = {};
    for (const assignment of assignments) {
      const m = assignment.match(/^(\w+)\s*=\s*(.+)$/i);
      if (!m) continue;
      // Evaluated per row, not once: "regenerateVersion = COALESCE(
      // regenerateVersion, 0) + 1" has to read the row it is updating, so
      // consecutive rows each increment from their own current value.
      const perRow = /\w*\s*\(|\b\w+\b\s*[+\-*/]\s*\S/.test(m[2]) && !/^\s*\?\s*$/.test(m[2]);
      if (!perRow) {
        updates[m[1]] = { __literal: m[2] };
      } else {
        updates[m[1]] = { __expr: m[2] };
      }
    }
    // Count the SET clause's placeholders directly rather than inferring it from
    // a cursor: literals and expressions consume parameters at different points
    // in the row loop, and getting this wrong mis-slices the WHERE parameters
    // and silently matches the wrong row.
    const setPlaceholders = countPlaceholders(setSql);
    const whereParams = params.slice(setPlaceholders);
    const where = buildWhere(whereSql, whereParams);
    let changes = 0;
    const changedRows = [];
    for (const row of rowsFor(table)) {
      if (!where(row)) continue;
      // Expressions are evaluated against the row as it stands, so a self-
      // reference sees the value it is replacing.
      const resolved = {};
      // Literals consume the same parameters on every row, so evaluate them
      // once. Expressions see the row and re-read the placeholders they contain.
      const literalCursor = { value: 0 };
      for (const [field, value] of Object.entries(updates)) {
        if (value.__literal === undefined) continue;
        resolved[field] = evalExpression(value.__literal, params, literalCursor, row).value;
      }
      for (const [field, value] of Object.entries(updates)) {
        if (value.__expr === undefined) continue;
        const c = { value: 0 };
        resolved[field] = evalExpression(value.__expr, params, c, row).value;
      }
      Object.assign(row, resolved);
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
    // One cursor across the whole statement so WHERE, LIMIT and OFFSET consume
    // their own placeholders in order.
    const cursor = { value: 0 };
    let rows = rowsFor(parsed.table).filter(buildWhere(parsed.where, params, cursor));

    // GROUP BY: bucket the filtered rows, then project one row per bucket.
    // Skipped entirely when absent, so a plain SELECT still returns one row per
    // matching record — except when the select list aggregates, where SQL
    // collapses the whole set to a single row.
    let grouped = null;
    const aggregatesWholeSet = !parsed.groupBy && containsAggregate(parsed.columns);
    if (parsed.groupBy) {
      const buckets = new Map();
      for (const row of rows) {
        const key = evalExpression(parsed.groupBy, params, { value: 0 }, row).value;
        // Map keys distinguish 1 from "1" and null from "null", so a group label
        // of two different types never merges.
        const bucketKey = `${typeof key}:${key}`;
        if (!buckets.has(bucketKey)) buckets.set(bucketKey, []);
        buckets.get(bucketKey).push(row);
      }
      grouped = [...buckets.values()];
    } else if (aggregatesWholeSet) {
      grouped = [rows];
    }

    if (parsed.order) {
      const terms = splitCsv(parsed.order).map((term) => {
        const [field, direction = "ASC"] = term.trim().split(/\s+/);
        return { field, direction: direction.toUpperCase() };
      });
      const sortKey = (row) => {
        // An ORDER BY may name the select alias ("ORDER BY requests") or the
        // underlying column; both have to work, because report queries use each.
        if (row[terms[0].field] !== undefined) return row[terms[0].field];
        if (grouped) {
          const first = grouped[0]?.[0];
          if (first) return first[terms[0].field];
        }
        return undefined;
      };
      rows = [...rows].sort((a, b) => {
        const av = sortKey(a);
        const bv = sortKey(b);
        if (av === bv) return 0;
        const result = av > bv ? 1 : -1;
        return terms[0].direction === "DESC" ? -result : result;
      });
    }

    // Read the placeholders in SQL order — LIMIT precedes OFFSET, so the bound
    // parameters do too. Applying them in the other order silently swapped them.
    const limit = parsed.limit
      ? Number(literalValue(parsed.limit, params, cursor).value)
      : null;
    const offset = parsed.offset
      ? Number(literalValue(parsed.offset, params, cursor).value)
      : 0;

    let result;
    if (grouped) {
      result = grouped.map((bucket) => projectGroup(bucket, parsed.columns, params));
    } else {
      // ...then slice in array order.
      if (Number.isFinite(offset) && offset > 0) rows = rows.slice(offset);
      if (Number.isFinite(limit)) rows = rows.slice(0, limit);
      // "*" and DISTINCT keep their dedicated handling; everything else goes
      // through the same projector as a grouped query, one row at a time, so a
      // per-row expression (substr(timestamp,1,10) AS day) projects instead of
      // being dropped as unparseable.
      result = parsed.columns === "*" || /^\s*DISTINCT\b/i.test(parsed.columns)
        ? projectRows(rows, parsed.columns)
        : rows.map((row) => projectGroup([row], parsed.columns, params));
    }

    if (grouped) {
      if (Number.isFinite(offset) && offset > 0) result = result.slice(offset);
      if (Number.isFinite(limit)) result = result.slice(0, limit);
    }
    return result;
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
