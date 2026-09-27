import { getAdapter } from "../driver.js";
import { parseJson, stringifyJson } from "./jsonCol.js";

export function makeKv(scope) {
  return {
    async get(key, fallback = null) {
      const db = await getAdapter();
      await db.ensureTables?.(["kv"]);
      const row = db.get(`SELECT value FROM kv WHERE scope = ? AND key = ?`, [scope, key]);
      return row ? parseJson(row.value, fallback) : fallback;
    },
    async getAll() {
      const db = await getAdapter();
      await db.ensureTables?.(["kv"]);
      const rows = db.all(`SELECT key, value FROM kv WHERE scope = ?`, [scope]);
      const out = {};
      for (const r of rows) {
        const parsed = parseJson(r.value);
        // parseJson returns null for a NULL or malformed column. Handing that
        // back as a "value" turns a single corrupt row into `null` at a
        // consumer's key, which then crashes on the first property access
        // (e.g. `null.startsWith` in ModelSelectModal). Skip unusable values
        // here so every consumer of every kv scope is safe, not just the ones
        // that remember to type-check.
        if (parsed === null || parsed === undefined) continue;
        out[r.key] = parsed;
      }
      return out;
    },
    async set(key, value) {
      const db = await getAdapter();
      await db.ensureTables?.(["kv"]);
      db.run(`INSERT INTO kv(scope, key, value) VALUES(?, ?, ?) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value`, [scope, key, stringifyJson(value)]);
    },
    async setMany(obj) {
      const db = await getAdapter();
      await db.ensureTables?.(["kv"]);
      db.transaction(() => {
        for (const [k, v] of Object.entries(obj)) {
          db.run(`INSERT INTO kv(scope, key, value) VALUES(?, ?, ?) ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value`, [scope, k, stringifyJson(v)]);
        }
      });
    },
    async remove(key) {
      const db = await getAdapter();
      await db.ensureTables?.(["kv"]);
      db.run(`DELETE FROM kv WHERE scope = ? AND key = ?`, [scope, key]);
    },
    async clear() {
      const db = await getAdapter();
      await db.ensureTables?.(["kv"]);
      db.run(`DELETE FROM kv WHERE scope = ?`, [scope]);
    },
  };
}
