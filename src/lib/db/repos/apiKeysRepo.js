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
  };
}

export async function getApiKeys(ownerRef) {
  const db = await getAdapter();
  const rows = ownerRef === undefined || ownerRef === null
    ? db.all(`SELECT * FROM apiKeys ORDER BY createdAt ASC`)
    : db.all(`SELECT * FROM apiKeys WHERE ownerRef = ? ORDER BY createdAt ASC`, [ownerRef]);
  return rows.map(rowToKey);
}

export async function getApiKeyById(id) {
  const db = await getAdapter();
  const row = db.get(`SELECT * FROM apiKeys WHERE id = ?`, [id]);
  return rowToKey(row);
}

export async function getApiKeyByKey(key) {
  const db = await getAdapter();
  const row = db.get(`SELECT * FROM apiKeys WHERE key = ?`, [key]);
  return rowToKey(row);
}

export async function createApiKey(name, machineId, options = {}) {
  if (!machineId) throw new Error("machineId is required");
  const db = await getAdapter();
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
  };
  db.run(
    `INSERT INTO apiKeys(${API_KEY_FIELDS.join(", ")}) VALUES(${API_KEY_FIELDS.map(() => "?").join(", ")})`,
    API_KEY_FIELDS.map(f => f === "isActive" ? (apiKey[f] ? 1 : 0) : apiKey[f])
  );
  return apiKey;
}

export async function updateApiKey(id, data) {
  const db = await getAdapter();
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
    } else if (data.key !== undefined) {
      data = { ...data };
      delete data.key;
    }
    const merged = { ...existing, ...data };
    const updateFields = API_KEY_FIELDS.filter(f => f !== "id");
    db.run(
      `UPDATE apiKeys SET ${updateFields.map(f => `${f} = ?`).join(", ")} WHERE id = ?`,
      [...updateFields.map(f => f === "isActive" ? (merged[f] ? 1 : 0) : merged[f]), id]
    );
    if (merged.key !== existing.key) {
      db.run(`UPDATE usageHistory SET apiKey = ? WHERE apiKey = ?`, [merged.key, existing.key]);
    }
    result = merged;
  });
  return result;
}

export async function regenerateApiKey(id) {
  const db = await getAdapter();
  let result = null;

  db.transaction(() => {
    const row = db.get(`SELECT * FROM apiKeys WHERE id = ?`, [id]);
    if (!row) return;

    const generated = generateApiKeyWithMachine(row.machineId);
    db.run(
      `UPDATE apiKeys SET key = ?, regenerateVersion = COALESCE(regenerateVersion, 0) + 1 WHERE id = ?`,
      [generated.key, id]
    );
    db.run(`UPDATE usageHistory SET apiKey = ? WHERE apiKey = ?`, [generated.key, row.key]);
    result = rowToKey(db.get(`SELECT * FROM apiKeys WHERE id = ?`, [id]));
  });

  return result;
}

export async function deleteApiKey(id) {
  const db = await getAdapter();
  const res = db.run(`DELETE FROM apiKeys WHERE id = ?`, [id]);
  return (res?.changes ?? 0) > 0;
}

export async function validateApiKey(key) {
  const db = await getAdapter();
  const row = db.get(`SELECT * FROM apiKeys WHERE key = ?`, [key]);
  if (!row) return false;
  if (row.isActive !== 1 && row.isActive !== true) return false;
  if (row.expiresAt && new Date(row.expiresAt) < new Date()) return false;
  return true;
}
