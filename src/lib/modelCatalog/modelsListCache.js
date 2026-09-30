import { getApiKeyByKey } from "@/lib/localDb";

// Auth and response policy for the /v1/models family.
//
// Three routes serve the catalogue: /v1/models, /v1/models/{kind} and
// /v1beta/models. They all do the same expensive thing — read every provider
// connection and build capability metadata for thousands of models — so they
// share this module to keep one auth decision and one response policy. Letting
// each route decide separately is how /v1beta/models ended up publicly readable
// while /v1/models was guarded.

/**
 * There is deliberately no server-side cache here.
 *
 * An earlier version held the built catalogue for 10 minutes to save work. That
 * was wrong for this endpoint: some applications treat it as the source of truth
 * for what they are allowed to call, and answering them with a list up to ten
 * minutes out of date is not something they can see or override. The caller
 * cannot tell that a response is stale, so the decision has to be theirs.
 *
 * Cost is handled without staleness instead:
 *   - capability derivation is memoized in open-sse/providers/capabilities.js.
 *     It is a pure function of (provider, model), so a memo cannot go stale.
 *   - concurrent identical rebuilds are coalesced, so a burst pays for one build
 *   - an unauthenticated caller is rejected before any of this runs
 *
 * Which leaves caching where it belongs: with the client. The Cache-Control on
 * the response says the body is per-caller, and `no-cache` lets a client store
 * it while requiring revalidation — so a client that wants to be fast can
 * revalidate, and one that needs absolute liveness always gets it.
 */

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

/**
 * Response headers for a catalogue body.
 *
 * `private` because the body is chosen for one caller: a restricted key sees
 * only its allow-list, and a shared cache must not be trusted with that.
 *
 * `no-cache` means the response may be stored but must be revalidated before
 * reuse. That is the honest instruction for an endpoint whose callers rely on it
 * being current — and it still leaves the choice to the client, which is where
 * caching belongs. A client that wants fewer round trips can ignore it.
 */
export function modelsListHeaders(extra = {}) {
  return {
    "Content-Type": "application/json; charset=utf-8",
    "Access-Control-Allow-Origin": "*",
    "Cache-Control": "private, no-cache, must-revalidate",
    "Vary": "Authorization, x-api-key, x-goog-api-key",
    ...extra,
  };
}

export function unauthorizedModelsResponse(error, status) {
  return Response.json({ error }, { status, headers: modelsListHeaders() });
}

/**
 * Collapse concurrent identical builds into one.
 *
 * Not a cache: the promise is dropped as soon as it resolves, so nothing is ever
 * reused after it settles and no caller can receive a stale body. It only stops
 * a burst — several clients starting at once, or one client retrying — from
 * paying for N identical builds.
 *
 * Reports whether this caller joined a build already in flight. With no response
 * cache that distinction is otherwise invisible, and it is the number that says
 * whether the CPU is being spent once per burst or once per request.
 *
 * Keyed by `cacheId` so a restricted caller's build is never handed to an
 * unrestricted caller, or the reverse.
 */
const inflight = new Map();

export async function coalesceModelsBuild(cacheId, build) {
  const existing = inflight.get(cacheId);
  if (existing) return { value: await existing, coalesced: true };
  const pending = (async () => build())();
  inflight.set(cacheId, pending);
  try {
    return { value: await pending, coalesced: false };
  } finally {
    if (inflight.get(cacheId) === pending) inflight.delete(cacheId);
  }
}
