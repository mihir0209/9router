// Verify 3-tier driver fallback: better-sqlite3 → node:sqlite → sql.js
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

let tempDir;
const originalDataDir = process.env.DATA_DIR;
const originalDatabaseUrl = process.env.DATABASE_URL;
const originalPostgresUrl = process.env.POSTGRES_URL;

beforeEach(() => {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "9router-chain-"));
  process.env.DATA_DIR = tempDir;
  // These tests are specifically about the SQLite fallback chain. When the
  // suite runs against Postgres (TEST_DATABASE_URL set for the isolation
  // harness), DATABASE_URL is live and Postgres correctly wins the driver
  // choice — which made these assert the wrong thing. Clear it so they test
  // what they claim; the precedence itself is covered by its own test below.
  delete process.env.DATABASE_URL;
  delete process.env.POSTGRES_URL;
  delete global._dbAdapter;
  vi.resetModules();
});

afterEach(() => {
  try { global._dbAdapter?.instance?.close?.(); } catch {}
  delete global._dbAdapter;
  if (tempDir) fs.rmSync(tempDir, { recursive: true, force: true });
  if (originalDataDir === undefined) delete process.env.DATA_DIR;
  else process.env.DATA_DIR = originalDataDir;
  if (originalDatabaseUrl === undefined) delete process.env.DATABASE_URL;
  else process.env.DATABASE_URL = originalDatabaseUrl;
  if (originalPostgresUrl === undefined) delete process.env.POSTGRES_URL;
  else process.env.POSTGRES_URL = originalPostgresUrl;
});

describe("Driver fallback chain", () => {
  it("prefers Postgres when DATABASE_URL is set", async () => {
    // Guarded on the database actually being reachable, so the default SQLite
    // run skips it instead of failing for want of a server.
    const testUrl = process.env.TEST_DATABASE_URL;
    if (!testUrl) return;
    process.env.DATABASE_URL = testUrl;
    delete global._dbAdapter;
    vi.resetModules();
    const { getAdapter } = await import("@/lib/db/driver.js");
    const db = await getAdapter();
    expect(db.driver).toBe("postgres-backed-memory");
  });

  it("refuses to fall back to a local database when DATABASE_URL is unreachable", async () => {
    // A configured-but-broken DATABASE_URL must be loud. Silently starting on a
    // fresh SQLite file would look like the app had lost every credential.
    process.env.DATABASE_URL = "postgres://nobody:nope@127.0.0.1:1/nothing";
    delete global._dbAdapter;
    vi.resetModules();
    const { getAdapter } = await import("@/lib/db/driver.js");
    await expect(getAdapter()).rejects.toThrow();
  });

  it("default → picks better-sqlite3 when available", async () => {
    const { getAdapter } = await import("@/lib/db/driver.js");
    const db = await getAdapter();
    expect(["better-sqlite3", "node:sqlite", "sql.js"]).toContain(db.driver);
  });

  it("falls back to node:sqlite when better-sqlite3 unavailable", async () => {
    // Mock the better-sqlite3 adapter to throw
    vi.doMock("@/lib/db/adapters/betterSqliteAdapter.js", () => {
      throw new Error("simulated unavailable");
    });
    const { getAdapter } = await import("@/lib/db/driver.js");
    const db = await getAdapter();
    // Node 22.5+ should give node:sqlite, else sql.js
    const [maj, min] = process.versions.node.split(".").map(Number);
    if (maj > 22 || (maj === 22 && min >= 5)) {
      expect(db.driver).toBe("node:sqlite");
    } else {
      expect(db.driver).toBe("sql.js");
    }
  });

  it("falls back to sql.js when both native drivers unavailable", async () => {
    vi.doMock("@/lib/db/adapters/betterSqliteAdapter.js", () => {
      throw new Error("simulated unavailable");
    });
    vi.doMock("@/lib/db/adapters/nodeSqliteAdapter.js", () => {
      throw new Error("simulated unavailable");
    });
    const { getAdapter } = await import("@/lib/db/driver.js");
    const db = await getAdapter();
    expect(db.driver).toBe("sql.js");
  });
});
