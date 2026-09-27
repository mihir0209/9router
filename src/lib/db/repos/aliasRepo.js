import { getAdapter } from "../driver.js";
import { parseJson, stringifyJson } from "../helpers/jsonCol.js";
import { makeKv } from "../helpers/kvStore.js";

const aliasKv = makeKv("modelAliases");
const customKv = makeKv("customModels");
const mitmKv = makeKv("mitmAlias");
const modelDisplayNamesKv = makeKv("modelDisplayNames");

// getAll() on a kv scope is a full scan of the kv table. At production scale
// that is ~3.5k rows and measures at ~5ms per call — and getModelInfo() calls
// this once per candidate model, so resolving a combo that expands to 23
// leaves cost ~119ms of CPU in a single request. Workers' CPU is far slower
// than local Node, which is enough to exhaust the request budget and hang.
//
// These are read on every model resolution and written only when an admin edits
// aliases, so a short per-isolate TTL cache is the right trade. Writes below
// invalidate it, so an edit takes effect immediately rather than after the TTL.
const KV_CACHE_TTL_MS = 5000;
const kvCache = new Map();

function cachedGetAll(scope, read) {
  const now = Date.now();
  const hit = kvCache.get(scope);
  if (hit && hit.expiresAt > now) return hit.value;
  return read().then((value) => {
    kvCache.set(scope, { value, expiresAt: Date.now() + KV_CACHE_TTL_MS });
    return value;
  });
}

function invalidateKvCache(scope) {
  kvCache.delete(scope);
}

export function resetKvCache() {
  kvCache.clear();
}

// modelAliases: key=alias, value=modelString
export async function getModelAliases() {
  return await cachedGetAll("modelAliases", () => aliasKv.getAll());
}

export async function setModelAlias(alias, model) {
  await aliasKv.set(alias, model);
  invalidateKvCache("modelAliases");
}

export async function deleteModelAlias(alias) {
  await aliasKv.remove(alias);
  invalidateKvCache("modelAliases");
}

// modelDisplayNames: key=fullModelId (e.g. "openai/gpt-4o"), value=friendlyName (e.g. "GPT-4o")
export async function getModelDisplayNames() {
  return await modelDisplayNamesKv.getAll();
}

export async function setModelDisplayName(modelId, displayName) {
  await modelDisplayNamesKv.set(modelId, displayName);
}

export async function deleteModelDisplayName(modelId) {
  await modelDisplayNamesKv.remove(modelId);
}

// customModels: key=`${providerAlias}|${id}|${type}`, value=full model object
function customKey(providerAlias, id, type) {
  return `${providerAlias}|${id}|${type}`;
}

export async function getCustomModels() {
  const all = await cachedGetAll("customModels", () => customKv.getAll());
  return Object.values(all);
}

// Atomic upsert inside transaction to prevent duplicate races.
// Re-adding an existing model updates caps/name/transport without resetting omitted fields.
export async function addCustomModel({ providerAlias, id, type = "llm", name, caps, transport }) {
  const k = customKey(providerAlias, id, type);
  const db = await getAdapter();
  let added = false;
  db.transaction(() => {
    const row = db.get(`SELECT value FROM kv WHERE scope = 'customModels' AND key = ?`, [k]);
    if (row) {
      const prev = parseJson(row.value) || {};
      const next = { ...prev, ...(name ? { name } : {}), ...(caps ? { caps } : {}), ...(transport ? { transport } : {}) };
      db.run(`UPDATE kv SET value = ? WHERE scope = 'customModels' AND key = ?`, [stringifyJson(next), k]);
      return;
    }
    const value = stringifyJson({ providerAlias, id, type, name: name || id, ...(caps ? { caps } : {}), ...(transport ? { transport } : {}) });
    db.run(`INSERT INTO kv(scope, key, value) VALUES('customModels', ?, ?)`, [k, value]);
    added = true;
  });
  invalidateKvCache("customModels");
  return added;
}

export async function deleteCustomModel({ providerAlias, id, type = "llm" }) {
  await customKv.remove(customKey(providerAlias, id, type));
  invalidateKvCache("customModels");
}

// mitmAlias: key=toolName, value=mappings object
export async function getMitmAlias(toolName) {
  if (toolName) {
    const v = await mitmKv.get(toolName);
    return v || {};
  }
  return await mitmKv.getAll();
}

export async function setMitmAliasAll(toolName, mappings) {
  await mitmKv.set(toolName, mappings || {});
}
