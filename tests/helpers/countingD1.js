// Counting fake of Cloudflare D1, backed by an in-memory node:sqlite database.
//
// Implements the slice of the D1 API the cloudflare adapter uses:
//   d1.prepare(sql).run() / .all()                 (unbound)
//   d1.prepare(sql).bind(...v).run() / .all() / .first()
//   d1.batch([stmt, ...])                          (one round trip, atomic)
// and counts what Cloudflare bills and what costs latency:
//   queries      statements executed (a batch of N statements = N)
//   roundTrips   calls into D1 (a batch = 1)
//   rowsRead     rows returned by SELECT (D1 bills rows scanned; for the
//                primary-key-prefix selects the adapter issues, scanned == returned)
//   rowsWritten  rows changed by INSERT/UPDATE/DELETE (sqlite changes()).
//                D1 additionally bills index maintenance; for a NEW row in
//                ninerouter_rows that is one more write for the automatic PK
//                index. rowsWrittenWorstCase doubles inserts to model that.
//
// This is a model, not Cloudflare's own meter: nothing here was compared with
// the `meta.rows_read` / `meta.rows_written` a real D1 returns.
import { DatabaseSync } from "node:sqlite";

function kindOf(sql) {
  const head = String(sql).trim().split(/\s+/)[0].toUpperCase();
  if (head === "SELECT") return "select";
  if (head === "INSERT" || head === "REPLACE") return "insert";
  if (head === "UPDATE") return "update";
  if (head === "DELETE") return "delete";
  return "ddl";
}

export function createCountingD1() {
  const db = new DatabaseSync(":memory:");
  const counters = { queries: 0, roundTrips: 0, rowsRead: 0, rowsWritten: 0, rowsWrittenWorstCase: 0, byKind: {} };
  const log = [];

  function bump(kind) {
    counters.byKind[kind] = (counters.byKind[kind] || 0) + 1;
  }

  function execute(sql, params, mode) {
    const kind = kindOf(sql);
    counters.queries++;
    bump(kind);
    const stmt = db.prepare(sql);
    let result;
    if (kind === "select") {
      const rows = stmt.all(...params);
      counters.rowsRead += rows.length;
      result = mode === "first" ? (rows[0] ?? null) : { results: rows, success: true, meta: { rows_read: rows.length, rows_written: 0 } };
    } else {
      const before = db.prepare("SELECT total_changes() AS n").get().n;
      stmt.run(...params);
      const changed = db.prepare("SELECT total_changes() AS n").get().n - before;
      counters.rowsWritten += changed;
      counters.rowsWrittenWorstCase += kind === "insert" ? changed * 2 : changed;
      result = { results: [], success: true, meta: { rows_read: 0, rows_written: changed } };
    }
    log.push({ kind, sql: String(sql).replace(/\s+/g, " ").slice(0, 100), params: params.slice(0, 4).map((p) => String(p).slice(0, 60)), n: kind === "select" ? (result.results?.length ?? (result ? 1 : 0)) : result.meta.rows_written });
    return result;
  }

  function makeStatement(sql, params = []) {
    return {
      sql,
      params,
      bind: (...values) => makeStatement(sql, values),
      run: async () => {
        counters.roundTrips++;
        return execute(sql, params, "run");
      },
      all: async () => {
        counters.roundTrips++;
        return execute(sql, params, "all");
      },
      first: async () => {
        counters.roundTrips++;
        return execute(sql, params, "first");
      },
      // batch() runs statements without counting an extra round trip each
      _exec: () => execute(sql, params, "run"),
    };
  }

  return {
    prepare: (sql) => makeStatement(sql),
    async batch(statements) {
      counters.roundTrips++;
      db.exec("BEGIN");
      try {
        const out = statements.map((s) => s._exec());
        db.exec("COMMIT");
        return out;
      } catch (error) {
        db.exec("ROLLBACK");
        throw error;
      }
    },
    /** Counters since creation or the last reset(). */
    snapshot: () => JSON.parse(JSON.stringify(counters)),
    reset() {
      counters.queries = 0;
      counters.roundTrips = 0;
      counters.rowsRead = 0;
      counters.rowsWritten = 0;
      counters.rowsWrittenWorstCase = 0;
      counters.byKind = {};
      log.length = 0;
    },
    log,
    /** Direct access for seeding / assertions; does NOT count. */
    raw: db,
    rowCount: (table) => db.prepare("SELECT COUNT(*) AS n FROM ninerouter_rows WHERE table_name = ?").get(table).n,
  };
}
