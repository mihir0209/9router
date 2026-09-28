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
