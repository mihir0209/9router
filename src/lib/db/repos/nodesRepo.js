import { v4 as uuidv4 } from "uuid";
import { getAdapter } from "../driver.js";
import { parseJson, stringifyJson } from "../helpers/jsonCol.js";

function rowToNode(row) {
  if (!row) return null;
  const extra = parseJson(row.data, {});
  return {
    ...extra,
    id: row.id,
    type: row.type,
    name: row.name,
    createdAt: row.createdAt,
    updatedAt: row.updatedAt,
  };
}

function nodeToRow(n) {
  const { id, type, name, createdAt, updatedAt, ...rest } = n;
  return {
    id,
    type: type ?? null,
    name: name ?? null,
    data: stringifyJson(rest),
    createdAt,
    updatedAt,
  };
}

function upsert(db, n) {
  const r = nodeToRow(n);
  db.run(
    `INSERT INTO providerNodes(id, type, name, data, createdAt, updatedAt)
     VALUES(?, ?, ?, ?, ?, ?)
     ON CONFLICT(id) DO UPDATE SET
       type=excluded.type, name=excluded.name, data=excluded.data, updatedAt=excluded.updatedAt`,
    [r.id, r.type, r.name, r.data, r.createdAt, r.updatedAt]
  );
}

const NODES_CACHE_TTL_MS = 5 * 60 * 1000;

if (!global._nodesRepoCache) {
  global._nodesRepoCache = { list: null, ts: 0 };
}
const nodesRepoCache = global._nodesRepoCache;

export function resetNodesCache() {
  nodesRepoCache.list = null;
  nodesRepoCache.ts = 0;
}

export async function getProviderNodes(filter = {}) {
  const now = Date.now();
  let list;
  if (nodesRepoCache.list && (now - nodesRepoCache.ts < NODES_CACHE_TTL_MS)) {
    list = nodesRepoCache.list;
  } else {
    const db = await getAdapter();
    if (typeof db.resync === "function") {
      await db.resync().catch(() => {});
    }
    const rows = db.all("SELECT * FROM providerNodes");
    list = rows.map(rowToNode);
    nodesRepoCache.list = list;
    nodesRepoCache.ts = now;
  }

  let result = list;
  if (filter.type) {
    result = list.filter((n) => n.type === filter.type);
  }
  return result.map((n) => ({ ...n }));
}

export async function getProviderNodeById(id) {
  const all = await getProviderNodes();
  const found = all.find((n) => n.id === id);
  return found ? { ...found } : null;
}

export async function createProviderNode(data) {
  const db = await getAdapter();
  const now = new Date().toISOString();
  const node = {
    id: data.id || uuidv4(),
    type: data.type,
    name: data.name,
    prefix: data.prefix,
    apiType: data.apiType,
    baseUrl: data.baseUrl,
    endpointUrl: data.endpointUrl,
    mediaKind: data.mediaKind,
    authHeader: data.authHeader,
    models: Array.isArray(data.models) ? data.models : undefined,
    customHeaders: data.customHeaders,
    createdAt: now,
    updatedAt: now,
  };
  if (data.retry != null) node.retry = data.retry;
  upsert(db, node);
  resetNodesCache();
  return node;
}

export async function updateProviderNode(id, data) {
  const db = await getAdapter();
  let result = null;
  db.transaction(() => {
    const row = db.get(`SELECT * FROM providerNodes WHERE id = ?`, [id]);
    if (!row) return;
    const merged = { ...rowToNode(row), ...data, updatedAt: new Date().toISOString() };
    upsert(db, merged);
    result = merged;
  });
  resetNodesCache();
  return result;
}

export async function deleteProviderNode(id) {
  const db = await getAdapter();
  let removed = null;
  db.transaction(() => {
    const row = db.get(`SELECT * FROM providerNodes WHERE id = ?`, [id]);
    if (!row) return;
    removed = rowToNode(row);
    db.run(`DELETE FROM providerNodes WHERE id = ?`, [id]);
  });
  if (removed) resetNodesCache();
  return removed;
}
