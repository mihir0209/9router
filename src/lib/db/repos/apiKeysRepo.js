import { v4 as uuidv4 } from "uuid";
import { getAdapter } from "../driver.js";

const API_KEY_FIELDS = [
  "id", "key", "name", "machineId", "isActive", "createdAt",
  "rateLimitRequests", "rateLimitWindowSeconds",
  "tokenLimitDaily", "tokenLimitMonthly",
  "allowedModels", "blockedModels",
  "allowedIPs", "blockedIPs",
  "expiresAt", "description",
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
  };
}

export async function getApiKeys() {
  const db = await getAdapter();
  const rows = db.all(`SELECT * FROM apiKeys ORDER BY createdAt ASC`);
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
  const { generateApiKeyWithMachine } = await import("@/shared/utils/apiKey");
  const result = generateApiKeyWithMachine(machineId);
  const apiKey = {
    id: uuidv4(),
    name,
    key: result.key,
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
    const merged = { ...existing, ...data };
    const updateFields = API_KEY_FIELDS.filter(f => f !== "id");
    db.run(
      `UPDATE apiKeys SET ${updateFields.map(f => `${f} = ?`).join(", ")} WHERE id = ?`,
      [...updateFields.map(f => f === "isActive" ? (merged[f] ? 1 : 0) : merged[f]), id]
    );
    result = merged;
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
