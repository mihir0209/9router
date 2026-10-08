import { v4 as uuidv4 } from "uuid";
import { getAdapter } from "../driver.js";
import { generateApiKeyWithMachine } from "@/shared/utils/apiKey";

const API_KEY_FIELDS = [
  "id", "key", "name", "machineId", "isActive", "createdAt",
  "rateLimitRequests", "rateLimitWindowSeconds",
  "tokenLimitDaily", "tokenLimitMonthly",
  "allowedModels", "blockedModels",
  "allowedIPs", "blockedIPs",
  "expiresAt", "description",
  "ownerRef", "budgetUsd", "budgetResetHours", "budgetWindowStartedAt",
  "billingMode", "byokProvider", "byokCredentialsCiphertext",
  "byokCredsEncVersion", "regenerateVersion",
  "maxConcurrentRequests", "allowedProviders", "blockedProviders",
  "forceProvider", "forceModel",
  "allowedEndpoints", "blockedEndpoints", "metadata", "updatedAt",
];

function rowToKey(row) {
  if (!row) return null;
  return {
    id: row.id,
    key: row.key,
    name: row.name,
    machineId: row.machineId,
    isActive: row.isActive === 1 || row.isActive === true,
    createdAt: row.createdAt,
    rateLimitRequests: row.rateLimitRequests || 0,
    rateLimitWindowSeconds: row.rateLimitWindowSeconds || 60,
    tokenLimitDaily: row.tokenLimitDaily || 0,
    tokenLimitMonthly: row.tokenLimitMonthly || 0,
    allowedModels: row.allowedModels || "",
    blockedModels: row.blockedModels || "",
    allowedIPs: row.allowedIPs || "",
    blockedIPs: row.blockedIPs || "",
    expiresAt: row.expiresAt || "",
    description: row.description || "",
    ownerRef: row.ownerRef ?? null,
    budgetUsd: row.budgetUsd ?? null,
    budgetResetHours: row.budgetResetHours ?? null,
    budgetWindowStartedAt: row.budgetWindowStartedAt ?? null,
    billingMode: row.billingMode || "credits",
    byokProvider: row.byokProvider ?? null,
    byokCredentialsCiphertext: row.byokCredentialsCiphertext ?? null,
    byokCredsEncVersion: row.byokCredsEncVersion ?? null,
    regenerateVersion: row.regenerateVersion ?? 0,
    maxConcurrentRequests: row.maxConcurrentRequests || 0,
    allowedProviders: row.allowedProviders || "",
    blockedProviders: row.blockedProviders || "",
    forceProvider: row.forceProvider || "",
    forceModel: row.forceModel || "",
    allowedEndpoints: row.allowedEndpoints || "",
    blockedEndpoints: row.blockedEndpoints || "",
    metadata: (() => { try { return JSON.parse(row.metadata || "{}"); } catch { return {}; } })(),
    updatedAt: row.updatedAt || row.createdAt,
  };
}

const API_KEYS_CACHE_TTL_MS = 5 * 60 * 1000;
const CONTROL_DOMAIN = "apiKeys";

if (!global._apiKeysRepoCache) {
  global._apiKeysRepoCache = { byKey: new Map(), byId: new Map(), list: null, ts: 0, version: null };
}
const apiKeyCache = global._apiKeysRepoCache;

export function resetApiKeysCache() {
  apiKeyCache.byKey.clear();
  apiKeyCache.byId.clear();
  apiKeyCache.list = null;
  apiKeyCache.ts = 0;
  apiKeyCache.version = null;
}

/**
 * Drop cached keys when another isolate created, updated, disabled or rotated
 * one. Without this a revoked key keeps authenticating on every other Workers
 * isolate for the full cache TTL. Clearing the repo cache is not enough: the
 * Cloudflare adapter holds its own copy of the table, so a version change must
 * also force that refresh or the next read repopulates from the same stale data.
 */
async function invalidateIfStale() {
  if (!apiKeyCache.byKey.size && !apiKeyCache.byId.size) return;
  const { getControlVersion } = await import("@/lib/coordinator/controlVersion.js");
  const version = await getControlVersion(CONTROL_DOMAIN);
  if (version == null) return;
  if (apiKeyCache.version == null) {
    apiKeyCache.version = version;
    return;
  }
  if (version === apiKeyCache.version) return;
  resetApiKeysCache();
  try {
    const db = await getAdapter();
    if (typeof db.resync === "function") await db.resync(true, ["apiKeys"]);
  } catch {}
}

async function noteControlVersion() {
  const { getControlVersion } = await import("@/lib/coordinator/controlVersion.js");
  apiKeyCache.version = await getControlVersion(CONTROL_DOMAIN);
}

/**
 * Publish a key change so other isolates drop their cached copy. Awaited: an
 * admin rotation that is not announced leaves the old key usable elsewhere.
 */
async function publishControlVersion() {
  resetApiKeysCache();
  const { bumpControlVersion } = await import("@/lib/coordinator/controlVersion.js");
  await bumpControlVersion(CONTROL_DOMAIN);
}

/**
 * On Workers each isolate keeps its own copy of the apiKeys table, refreshed
 * from D1 only every few minutes. Key writes must merge against current state,
 * otherwise a stale isolate can miss a duplicate-key clash, resurrect a
 * pre-rotation secret, or re-enable a key that was just disabled. These writes
 * are rare admin operations, so a forced refresh is cheap enough.
 */
async function adapterForWrite() {
  const db = await getAdapter();
  if (typeof db.resync === "function") {
    await db.resync(true, ["apiKeys"]).catch(() => {});
  }
  return db;
}

export async function getApiKeys(ownerRef) {
  const db = await getAdapter();
  const rows = ownerRef === undefined || ownerRef === null
    ? db.all(`SELECT * FROM apiKeys ORDER BY createdAt ASC`)
    : db.all(`SELECT * FROM apiKeys WHERE ownerRef = ? ORDER BY createdAt ASC`, [ownerRef]);
  return rows.map(rowToKey);
}

export async function getApiKeyById(id) {
  if (!id) return null;
  await invalidateIfStale();
  const now = Date.now();
  if (apiKeyCache.byId.has(id) && (now - apiKeyCache.ts < API_KEYS_CACHE_TTL_MS)) {
    const k = apiKeyCache.byId.get(id);
    return k ? { ...k } : null;
  }
  const db = await getAdapter();
  // See settingsRepo.readRaw: on Workers the adapter answers from its own
  // in-memory copy, so a cold read must refresh D1 first. Otherwise this
  // isolate caches stale data and stamps it with the current control version,
  // convincing itself it is fresh until the adapter's own timer fires.
  if (process.env.NINEROUTER_RUNTIME === "cloudflare" && typeof db.resync === "function") {
    await db.resync(true, ["apiKeys"]).catch(() => {});
  }
  const row = db.get(`SELECT * FROM apiKeys WHERE id = ?`, [id]);
  const parsed = rowToKey(row);
  if (now - apiKeyCache.ts >= API_KEYS_CACHE_TTL_MS) {
    apiKeyCache.byKey.clear();
    apiKeyCache.byId.clear();
    apiKeyCache.ts = now;
  }
  if (parsed) {
    apiKeyCache.byId.set(id, parsed);
    apiKeyCache.byKey.set(parsed.key, parsed);
    await noteControlVersion();
  }
  return parsed ? { ...parsed } : null;
}

export async function getApiKeyByKey(key) {
  if (!key) return null;
  await invalidateIfStale();
  const now = Date.now();
  if (apiKeyCache.byKey.has(key) && (now - apiKeyCache.ts < API_KEYS_CACHE_TTL_MS)) {
    const k = apiKeyCache.byKey.get(key);
    return k ? { ...k } : null;
  }
  const db = await getAdapter();
  // See settingsRepo.readRaw: on Workers the adapter answers from its own
  // in-memory copy, so a cold read must refresh D1 first. Otherwise this
  // isolate caches stale data and stamps it with the current control version,
  // convincing itself it is fresh until the adapter's own timer fires.
  if (process.env.NINEROUTER_RUNTIME === "cloudflare" && typeof db.resync === "function") {
    await db.resync(true, ["apiKeys"]).catch(() => {});
  }
  const row = db.get(`SELECT * FROM apiKeys WHERE key = ?`, [key]);
  const parsed = rowToKey(row);
  if (now - apiKeyCache.ts >= API_KEYS_CACHE_TTL_MS) {
    apiKeyCache.byKey.clear();
    apiKeyCache.byId.clear();
    apiKeyCache.ts = now;
  }
  if (parsed) {
    apiKeyCache.byKey.set(key, parsed);
    apiKeyCache.byId.set(parsed.id, parsed);
    await noteControlVersion();
  }
  return parsed ? { ...parsed } : null;
}

export async function createApiKey(name, machineId, options = {}) {
  if (!machineId) throw new Error("machineId is required");
  const db = await adapterForWrite();
  let keyValue;
  const customKey = options.key !== undefined && options.key !== null ? String(options.key).trim() : "";
  if (customKey) {
    if (customKey.length > 512) throw new Error("API key must be 512 characters or fewer");
    const clash = db.get(`SELECT id FROM apiKeys WHERE key = ?`, [customKey]);
    if (clash) {
      const err = new Error("API key already in use");
      err.code = "DUPLICATE_API_KEY";
      throw err;
    }
    keyValue = customKey;
  } else {
    const { generateApiKeyWithMachine } = await import("@/shared/utils/apiKey");
    keyValue = generateApiKeyWithMachine(machineId).key;
  }
  const apiKey = {
    id: uuidv4(),
    name,
    key: keyValue,
    machineId,
    isActive: true,
    createdAt: new Date().toISOString(),
    rateLimitRequests: options.rateLimitRequests || 0,
    rateLimitWindowSeconds: options.rateLimitWindowSeconds || 60,
    tokenLimitDaily: options.tokenLimitDaily || 0,
    tokenLimitMonthly: options.tokenLimitMonthly || 0,
    allowedModels: options.allowedModels || "",
    blockedModels: options.blockedModels || "",
    allowedIPs: options.allowedIPs || "",
    blockedIPs: options.blockedIPs || "",
    expiresAt: options.expiresAt || "",
    description: options.description || "",
    ownerRef: options.ownerRef ?? null,
    budgetUsd: options.budgetUsd ?? null,
    budgetResetHours: options.budgetResetHours ?? null,
    budgetWindowStartedAt: options.budgetWindowStartedAt ?? null,
    billingMode: options.billingMode || "credits",
    byokProvider: options.byokProvider ?? null,
    byokCredentialsCiphertext: options.byokCredentialsCiphertext ?? null,
    byokCredsEncVersion: options.byokCredsEncVersion ?? null,
    regenerateVersion: options.regenerateVersion ?? 0,
    maxConcurrentRequests: options.maxConcurrentRequests || 0,
    allowedProviders: options.allowedProviders || "",
    blockedProviders: options.blockedProviders || "",
    forceProvider: typeof options.forceProvider === "string" ? options.forceProvider.trim() : (options.forceProvider || ""),
    forceModel: typeof options.forceModel === "string" ? options.forceModel.trim() : (options.forceModel || ""),
    allowedEndpoints: options.allowedEndpoints || "",
    blockedEndpoints: options.blockedEndpoints || "",
    metadata: typeof options.metadata === "string" ? options.metadata : JSON.stringify(options.metadata || {}),
    updatedAt: new Date().toISOString(),
  };
  db.run(
    `INSERT INTO apiKeys(${API_KEY_FIELDS.join(", ")}) VALUES(${API_KEY_FIELDS.map(() => "?").join(", ")})`,
    API_KEY_FIELDS.map(f => f === "isActive" ? (apiKey[f] ? 1 : 0) : apiKey[f])
  );
  // A local write makes the local cache stale, full stop. The control version
  // below only moves when a coordinator is recording it — on SQLite and on
  // Postgres without a Durable Object it does not — so relying on it left a
  // read immediately after a write returning the pre-write row.
  resetApiKeysCache();
  await publishControlVersion();
  return apiKey;
}

export async function updateApiKey(id, data) {
  const db = await adapterForWrite();
  // usageHistory is a lazy table. The transaction body below is synchronous and
  // cannot await, so load it up front when this call is about to remap keys.
  if (data?.key !== undefined && data?.key !== null && String(data.key).trim() !== "") {
    await db.ensureTables?.(["usageHistory"]);
  }
  let result = null;
  db.transaction(() => {
    const row = db.get(`SELECT * FROM apiKeys WHERE id = ?`, [id]);
    if (!row) return;
    const existing = rowToKey(row);
    if (data.key !== undefined && data.key !== null && String(data.key).trim() !== "" && String(data.key).trim() !== existing.key) {
      const nextKey = String(data.key).trim();
      if (nextKey.length > 512) throw new Error("API key must be 512 characters or fewer");
      const clash = db.get(`SELECT id FROM apiKeys WHERE key = ?`, [nextKey]);
      if (clash && clash.id !== id) {
        const err = new Error("API key already in use");
        err.code = "DUPLICATE_API_KEY";
        throw err;
      }
      data = { ...data, key: nextKey };
      // Remap the denormalised `apiKey` column so a rotated key still displays
      // under its new value. The `apiKeyId IS NULL` guard that used to be here
      // is obsolete: saveRequestUsage now stamps apiKeyId, so every attributed
      // row was excluded and history kept rendering the pre-rotation mask.
      // Attribution itself is stable via apiKeyId; this only fixes the label.
      db.run(
        `UPDATE usageHistory SET apiKey = ? WHERE (apiKey = ? OR (apiKeyId IS NOT NULL AND apiKeyId = ?))`,
        [nextKey, existing.key, id]
      );
    } else if (data.key !== undefined) {
      data = { ...data };
      delete data.key;
    }

    // Write only the columns the caller actually supplied. Rewriting the whole
    // row from a pre-read snapshot would resurrect values that changed in the
    // meantime — most dangerously, an `updateApiKey({description})` landing
    // after a rotation would write the pre-rotation key straight back.
    const now = new Date().toISOString();
    const assigned = API_KEY_FIELDS.filter((field) => field !== "id" && field !== "updatedAt" && data[field] !== undefined);
    const columns = [...assigned, "updatedAt"];

    db.run(
      `UPDATE apiKeys SET ${columns.map((f) => `${f} = ?`).join(", ")} WHERE id = ?`,
      [...columns.map((f) => {
        if (f === "updatedAt") return now;
        if (f === "isActive") return data[f] ? 1 : 0;
        if (f === "metadata") return typeof data[f] === "string" ? data[f] : JSON.stringify(data[f] || {});
        return data[f];
      }), id]
    );

    result = rowToKey(db.get(`SELECT * FROM apiKeys WHERE id = ?`, [id]));
  });
  // A local write makes the local cache stale, full stop. The control version
  // below only moves when a coordinator is recording it — on SQLite and on
  // Postgres without a Durable Object it does not — so relying on it left a
  // read immediately after a write returning the pre-write row.
  resetApiKeysCache();
  await publishControlVersion();
  return result;
}

export async function regenerateApiKey(id) {
  const db = await adapterForWrite();
  let result = null;

  db.transaction(() => {
    const row = db.get(`SELECT * FROM apiKeys WHERE id = ?`, [id]);
    if (!row) return;

    const generated = generateApiKeyWithMachine(row.machineId);
    db.run(
      `UPDATE apiKeys SET key = ?, regenerateVersion = COALESCE(regenerateVersion, 0) + 1, updatedAt = ? WHERE id = ?`,
      [generated.key, new Date().toISOString(), id]
    );
    result = rowToKey(db.get(`SELECT * FROM apiKeys WHERE id = ?`, [id]));
  });

  // A local write makes the local cache stale, full stop. The control version
  // below only moves when a coordinator is recording it — on SQLite and on
  // Postgres without a Durable Object it does not — so relying on it left a
  // read immediately after a write returning the pre-write row.
  resetApiKeysCache();
  await publishControlVersion();
  return result;
}

export async function deleteApiKey(id) {
  const db = await adapterForWrite();
  const res = db.run(`DELETE FROM apiKeys WHERE id = ?`, [id]);
  const deleted = (res?.changes ?? 0) > 0;
  if (deleted) await publishControlVersion();
  return deleted;
}

export async function validateApiKey(key) {
  const row = await getApiKeyByKey(key);
  if (!row) return false;
  if (row.isActive !== 1 && row.isActive !== true) return false;
  if (row.expiresAt && new Date(row.expiresAt) < new Date()) return false;
  return true;
}
