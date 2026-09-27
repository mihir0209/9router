import { makeKv } from "../helpers/kvStore.js";

// Live-discovered model ids, cached per provider so /v1/models can answer from
// the database instead of fanning out to every upstream on each request.
//
// Why this exists: building the model list live calls each provider in turn
// (LIVE_MODEL_RESOLVERS / fetchCompatibleModelIds). That is a round trip per
// provider, so a single /v1/models costs N sequential-ish upstream calls and
// holds the request open for seconds. On Cloudflare it is worse: that work
// happens per isolate and burns the free-plan CPU budget.
//
// Safety cap only. Some aggregators legitimately expose thousands of models
// (one here returns 2000+), so a low cap would silently drop real models from
// /v1/models and break routing for them. This exists purely so a pathological
// response cannot grow a row without bound; when it ever trips, the write
// reports `truncated` so the caller can say so rather than quietly lying.
const cacheKv = makeKv("providerModelCache");
const MAX_CACHED_MODELS_PER_PROVIDER = 20000;

/** Normalize one cached/live model entry to the shape the list builder expects. */
function normalizeModelEntry(model) {
  const id = typeof model === "string" ? model : model?.id;
  if (typeof id !== "string" || id.trim() === "") return null;
  const entry = { id: id.trim() };
  if (typeof model?.name === "string" && model.name) entry.name = model.name;
  if (typeof model?.kind === "string" && model.kind) entry.kind = model.kind;
  if (model?.capabilities && typeof model.capabilities === "object") {
    entry.capabilities = model.capabilities;
  }
  return entry;
}

/**
 * Read one provider's cached model list.
 * @returns {{models: object[], fetchedAt: string|null}|null} null when never refreshed.
 */
export async function getProviderModelCache(providerId) {
  if (!providerId) return null;
  const raw = await cacheKv.get(providerId);
  if (!raw || typeof raw !== "object") return null;
  const models = Array.isArray(raw.models) ? raw.models.map(normalizeModelEntry).filter(Boolean) : [];
  return { models, fetchedAt: typeof raw.fetchedAt === "string" ? raw.fetchedAt : null };
}

/** Read every cached provider entry, keyed by provider id. */
export async function getAllProviderModelCaches() {
  const all = await cacheKv.getAll();
  const out = {};
  for (const [providerId, raw] of Object.entries(all)) {
    if (!raw || typeof raw !== "object") continue;
    const models = Array.isArray(raw.models) ? raw.models.map(normalizeModelEntry).filter(Boolean) : [];
    out[providerId] = {
      models,
      fetchedAt: typeof raw.fetchedAt === "string" ? raw.fetchedAt : null,
    };
  }
  return out;
}

/**
 * Replace a provider's cached model list. A refresh that resolves zero models
 * is treated as a failure and does NOT overwrite a previously good cache —
 * otherwise one bad upstream response silently empties the list.
 * @returns {{stored: boolean, count: number, reason?: string}}
 */
export async function saveProviderModelCache(providerId, models, { fetchedAt } = {}) {
  if (!providerId) return { stored: false, count: 0, reason: "no-provider" };
  const normalized = (Array.isArray(models) ? models : []).map(normalizeModelEntry).filter(Boolean);
  const unique = Array.from(new Map(normalized.map((m) => [m.id, m])).values());
  if (unique.length === 0) {
    return { stored: false, count: 0, reason: "empty" };
  }
  const capped = unique.slice(0, MAX_CACHED_MODELS_PER_PROVIDER);
  await cacheKv.set(providerId, {
    models: capped,
    fetchedAt: fetchedAt || new Date().toISOString(),
  });
  return {
    stored: true,
    count: capped.length,
    truncated: unique.length > capped.length,
    totalDiscovered: unique.length,
  };
}

/** Drop one provider's cache, or every provider's when called with no id. */
export async function clearProviderModelCache(providerId) {
  if (providerId) return cacheKv.remove(providerId);
  return cacheKv.clear();
}
