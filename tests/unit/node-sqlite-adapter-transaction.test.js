import { describe, it, expect, beforeEach, afterEach } from "vitest";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { DatabaseSync } from "node:sqlite";
import { createNodeSqliteAdapter } from "../../src/lib/db/adapters/nodeSqliteAdapter.js";

// Several server processes share one WAL database. A read-then-write transaction that starts deferred
// fails at once with "database is locked" when another process commits in between, so the adapter must
// take the write lock when the outermost transaction begins.
describe("node:sqlite adapter transaction", () => {
  let dir;
  let adapter;
  let other;

  beforeEach(async () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), "9r-tx-"));
    const file = path.join(dir, "t.sqlite");
    adapter = await createNodeSqliteAdapter(file);
    adapter.exec("CREATE TABLE IF NOT EXISTS t (k TEXT PRIMARY KEY, v INTEGER)");
    other = new DatabaseSync(file);
    other.exec("PRAGMA busy_timeout = 0");
  });

  afterEach(() => {
    try { other.close(); } catch { /* closed */ }
    try { adapter.close(); } catch { /* closed */ }
    fs.rmSync(dir, { recursive: true, force: true });
  });

  it("holds the write lock from the first statement, even when the body only reads", () => {
    adapter.transaction(() => {
      adapter.get("SELECT v FROM t WHERE k = 'a'");
      expect(() => other.exec("INSERT INTO t VALUES('x', 1)")).toThrow(/locked|busy/i);
    });
    // Lock released on commit.
    other.exec("INSERT INTO t VALUES('x', 1)");
    expect(other.prepare("SELECT v FROM t WHERE k = 'x'").get().v).toBe(1);
  });

  it("commits on success and rolls back on error, returning the callback value", () => {
    expect(adapter.transaction(() => { adapter.run("INSERT INTO t VALUES('a', 1)"); return 42; })).toBe(42);
    expect(() => adapter.transaction(() => { adapter.run("INSERT INTO t VALUES('b', 2)"); throw new Error("boom"); })).toThrow("boom");
    expect(adapter.get("SELECT v FROM t WHERE k = 'a'").v).toBe(1);
    expect(adapter.get("SELECT v FROM t WHERE k = 'b'")).toBeUndefined();
    // The lock is not leaked by the failed transaction.
    other.exec("INSERT INTO t VALUES('c', 3)");
  });

  it("nested transactions use savepoints: an inner failure rolls back only the inner work", () => {
    adapter.transaction(() => {
      adapter.run("INSERT INTO t VALUES('outer', 1)");
      expect(() => adapter.transaction(() => { adapter.run("INSERT INTO t VALUES('inner', 2)"); throw new Error("inner"); })).toThrow("inner");
      adapter.run("INSERT INTO t VALUES('after', 3)");
    });
    const keys = adapter.all("SELECT k FROM t ORDER BY k").map((r) => r.k);
    expect(keys).toEqual(["after", "outer"]);
  });

  it("is usable again after a nested failure escapes the outer transaction", () => {
    expect(() => adapter.transaction(() => { adapter.transaction(() => { throw new Error("x"); }); })).toThrow("x");
    adapter.transaction(() => adapter.run("INSERT INTO t VALUES('ok', 1)"));
    expect(adapter.get("SELECT v FROM t WHERE k = 'ok'").v).toBe(1);
  });
});
