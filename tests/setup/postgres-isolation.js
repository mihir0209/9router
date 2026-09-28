// Per-file database isolation for the Postgres run.
//
// A shared database leaks state between test files: a row one file inserts is
// still there when the next one counts rows, which is how 42 of 3456 tests fail
// the moment DATABASE_URL points at a real server. Those tests are written
// against the SQLite path, where every process gets a fresh database file.
//
// So when TEST_DATABASE_URL is set, every test file gets its own Postgres
// schema, dropped again when the file finishes. Each file sees an empty
// database, which is exactly what the SQLite path gives them, and files running
// in parallel cannot see each other's rows.
//
// This is a no-op unless TEST_DATABASE_URL is set, so the default SQLite run is
// completely unaffected.
//
// It deliberately never reads DATABASE_URL: these tests recreate schemas, so
// pointing them at a real database would destroy it. See tests/helpers/pg.sh.

import { afterAll } from "vitest";
import { randomBytes } from "node:crypto";

const testUrl = process.env.TEST_DATABASE_URL;

if (testUrl) {
  const { Pool } = await import("pg");

  // Short and unique. Postgres identifiers cap at 63 bytes, so keep the
  // generated part small and put the file name first while there is room.
  const fileHint = (process.env.VITEST_POOL_ID || "t")
    .replace(/[^a-zA-Z0-9]/g, "")
    .slice(0, 16);
  const schema = `t_${fileHint}_${randomBytes(4).toString("hex")}`;

  const admin = new Pool({ connectionString: testUrl, max: 1 });
  await admin.query(`CREATE SCHEMA IF NOT EXISTS "${schema}"`);
  admin.end();

  // search_path is the supported way to pin a session to a schema, so every
  // statement the adapter issues — including the unqualified CREATE TABLE from
  // postgresSchema.js — lands in this file's schema without any code change.
  const separator = testUrl.includes("?") ? "&" : "?";
  process.env.DATABASE_URL = `${testUrl}${separator}options=-c%20search_path%3D${schema}`;

  afterAll(async () => {
    const cleanup = new Pool({ connectionString: testUrl, max: 1 });
    try {
      await cleanup.query(`DROP SCHEMA IF EXISTS "${schema}" CASCADE`);
    } catch {
      // A leaked schema is untidy, not a failure worth failing a test run over.
    } finally {
      cleanup.end();
    }
  });
}
