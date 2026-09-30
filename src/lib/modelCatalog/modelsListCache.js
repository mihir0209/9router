import { getApiKeyByKey } from "@/lib/localDb";

// Auth + cache for the /v1/models family.
//
// Three routes serve the catalogue: /v1/models, /v1/models/{kind} and
// /v1beta/models. They all do the same expensive thing — read every provider
// connection and build capability metadata for thousands of models — so they
// share this module to keep one auth decision and one cache policy. Leaving
// them to each decide was how /v1beta/models ended up publicly readable while
// /v1/models was guarded.

/**
 * How long a built catalogue is reused.
 *
 * 10 minutes. Every miss costs a full rebuild, and in live mode that includes a
 * fan-out to every provider. Most clients list models on start-up, so without
 * this a client restart pays for a rebuild. The list only changes when an admin
 * edits a connection or a combo, and 10 minutes of staleness is invisible next
 * to that.
 */
export const MODELS_LIST_TTL_MS = 600_000;
export const MODELS_EDGE_TTL_S = 600;

// Every unrestricted caller gets the same list, so they share one entry. A
// restricted caller gets their own, keyed by their key record's id, so an
// allow-list is never handed to someone else.
const UNRESTRICTED_CACHE_ID = "all";

const isCloudflareRuntime = () => process.env.NINEROUTER_RUNTIME === "cloudflare";

/** Read the caller's key from any of the headers clients actually send. */
export function offeredApiKey(request) {
  const auth = request?.headers?.get("Authorization");
  if (auth?.startsWith("Bearer ")) return auth.slice(7);
  return request?.headers?.get("x-api-key")
    || request?.headers?.get("x-goog-api-key")
    || request?.nextUrl?.searchParams?.get("key")
    || null;
}

/** The allow-list on a key, or null when the key is unrestricted. */
export function parseModelList(raw) {
  const list = Array.isArray(raw)
    ? raw.map((s) => String(s).trim()).filter(Boolean)
    : typeof raw === "string"
      ? raw.split(",").map((s) => s.trim()).filter(Boolean)
      : [];
  return list.length > 0 && !list.includes("*") ? list : null;
}

/**
 * Authenticate a catalogue request.
 *
 * Deliberately narrower than enforceApiKeyRestrictions: that charges budget,
 * takes a concurrency lease and enforces rate limits, none of which belong on a
 * catalogue read. A client that lists models on start-up would be 429'd or
 * billed for looking, and the list costs no provider tokens. What does apply is
 * that the key must exist, be active and be unexpired.
 *
 * This lives in the route rather than relying on the Cloudflare worker guard so
 * the requirement holds on local, Docker and Antideploy deployments too, which
 * have no such guard.
 */
export async function authenticateModelsRequest(request) {
  const offeredKey = offeredApiKey(request);
  if (!offeredKey) {
    return { ok: false, status: 401, error: "API key required" };
  }
  const keyRecord = await getApiKeyByKey(offeredKey).catch(() => null);
  if (!keyRecord) {
    return { ok: false, status: 401, error: "Invalid API key" };
  }
  if (!keyRecord.isActive) {
    return { ok: false, status: 401, error: "API key is disabled" };
  }
  if (keyRecord.expiresAt) {
    const expiresAt = new Date(keyRecord.expiresAt);
    if (Number.isFinite(expiresAt.getTime()) && expiresAt <= new Date()) {
      return { ok: false, status: 401, error: "API key has expired" };
    }
  }
  return { ok: true, keyRecord };
}

export function unauthorizedModelsResponse(error, status) {
  return Response.json({ error }, { status, headers: modelsListHeaders() });
}

/**
 * Response headers for a catalogue body.
 *
 * `private`, not `public`: the body is chosen for one caller, and `public`
 * would let any shared cache in front of this app store an authenticated
 * response and serve it to the next caller. That is the same leak the per-key
 * cache ids exist to prevent, one layer further out.
 */
export function modelsListHeaders(extra = {}) {
  return {
    "Content-Type": "application/json; charset=utf-8",
    "Access-Control-Allow-Origin": "*",
    "Cache-Control": "private, max-age=60",
    "Vary": "Authorization, x-api-key, x-goog-api-key",
    ...extra,
  };
}

/** The cache entry identity for a caller: shared when unrestricted, per-key otherwise. */
export function modelsCacheId(keyRecord) {
  const allowed = parseModelList(keyRecord?.allowedModels);
  return allowed ? `k:${keyRecord.id}` : UNRESTRICTED_CACHE_ID;
}

// Isolate-local cache: cacheId -> { body, ts, inflight }.
// A Map rather than one slot because restricted keys each need an entry, and
// bounded because the key count is not — key rotation would otherwise grow this
// until the isolate ran out of memory.
const MODELS_MEMORY_ENTRIES_MAX = 64;
if (!global._modelsListCache) {
  global._modelsListCache = new Map();
}

export function memoryEntry(cacheId) {
  const cache = global._modelsListCache;
  let entry = cache.get(cacheId);
  if (!entry) {
    entry = { body: null, ts: 0, inflight: null };
    cache.set(cacheId, entry);
    // Map preserves insertion order, so the first key is the oldest.
    while (cache.size > MODELS_MEMORY_ENTRIES_MAX) {
      const oldest = cache.keys().next().value;
      if (oldest === cacheId) break; // never evict the entry we just created
      cache.delete(oldest);
    }
  }
  return entry;
}

function edgeCacheKey(request, cacheId) {
  // The identity has to travel in the query string: `caches.default` derives its
  // key from URL + method + Vary, not from arbitrary headers, so two keys asking
  // the same URL would otherwise share one entry. It is a uuid, not the secret.
  const url = new URL("/v1/models", request.url);
  url.searchParams.set("_ck", cacheId);
  return new Request(url.href, {
    method: "GET",
    headers: { accept: "application/json" },
  });
}

export async function readModelsEdgeCache(request, cacheId) {
  if (!isCloudflareRuntime()) return null;
  try {
    const hit = await caches.default.match(edgeCacheKey(request, cacheId));
    if (!hit) return null;
    return (await hit.text()) || null;
  } catch {
    return null;
  }
}

export async function writeModelsEdgeCache(request, cacheId, body) {
  if (!isCloudflareRuntime() || !body) return;
  try {
    const res = new Response(body, {
      headers: {
        "Content-Type": "application/json; charset=utf-8",
        // The stored copy lives the full 10 minutes. This header is on the
        // cached object only; the response returned to the caller is built
        // separately and stays `private`.
        "Cache-Control": `public, max-age=${MODELS_EDGE_TTL_S}`,
      },
    });
    await caches.default.put(edgeCacheKey(request, cacheId), res);
  } catch {
    /* edge cache is best-effort */
  }
}

/**
 * Serve a cached catalogue, or build and cache it.
 *
 * `build` is only called on a miss, and concurrent misses for the same identity
 * share one build, so a burst of clients costs one rebuild rather than N.
 */
export async function withModelsCache({ request, cacheId, skipCache, build }) {
  const entry = memoryEntry(cacheId);
  const now = Date.now();

  if (!skipCache && entry.body && now - entry.ts < MODELS_LIST_TTL_MS) {
    return { body: entry.body, source: "memory" };
  }

  if (!skipCache) {
    const edgeBody = await readModelsEdgeCache(request, cacheId);
    if (edgeBody) {
      entry.body = edgeBody;
      entry.ts = Date.now();
      return { body: edgeBody, source: "edge" };
    }
    if (entry.inflight) {
      return { body: await entry.inflight, source: "coalesced" };
    }
  }

  const pending = (async () => JSON.stringify(await build()))();
  if (!skipCache) entry.inflight = pending;

  let body;
  try {
    body = await pending;
  } finally {
    if (entry.inflight === pending) entry.inflight = null;
  }

  if (!skipCache && body) {
    entry.body = body;
    entry.ts = Date.now();
    await writeModelsEdgeCache(request, cacheId, body);
  }
  return { body, source: "miss" };
}
