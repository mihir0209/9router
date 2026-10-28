// Keep the suite off the developer's real data directory.
//
// A handful of tests (codex-stt-persisted-refresh, gemini-native-endpoint,
// models-auth-and-live, ...) reach the SQLite layer without setting DATA_DIR,
// so they opened and migrated the real ~/.9router/db/data.sqlite. That is
// destructive on a developer machine and fails with "attempt to write a
// readonly database" wherever that directory is read-only.
//
// When DATA_DIR is not set, give every test file its own throwaway directory.
// Tests that set DATA_DIR themselves are unaffected (they overwrite it and
// restore whatever they saw, which is this directory).

import { afterAll } from "vitest";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

if (!process.env.DATA_DIR) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "9router-test-data-"));
  process.env.DATA_DIR = dir;
  const cleanup = () => fs.rmSync(dir, { recursive: true, force: true });
  process.once("exit", cleanup);
  afterAll(() => {
    try { global._dbAdapter?.instance?.close?.(); } catch {}
    cleanup();
    if (process.env.DATA_DIR === dir) delete process.env.DATA_DIR;
  });
}
