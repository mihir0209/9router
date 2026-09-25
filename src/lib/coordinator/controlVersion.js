/**
 * Cross-isolate cache invalidation for security-critical state.
 *
 * The settings and API-key repos cache for minutes to keep the hot request path
 * off the database. In a single Node process that is safe: every write happens
 * in the same process and resets the local cache. On Cloudflare Workers each
 * isolate has its own module instance and its own copy of the cache, so a kill
 * switch flip or an API-key rotation is invisible to other isolates until the
 * TTL expires.
 *
 * Fix: writers bump a monotonic version for a domain inside the Durable Object,
 * and readers poll that version. The poll is throttled per isolate, so the cost
 * is one subrequest every POLL_INTERVAL_MS — not one per request.
 */

const POLL_INTERVAL_MS = 2_000;
const SCOPE = "control";

const domains = new Map();

function isCloudflare() {
  return process.env.NINEROUTER_RUNTIME === "cloudflare";
}

async function coordinatorCall(method, payload) {
  const { coordinatorCall: call } = await import("@/lib/coordinator/client.js");
  return await call(method, payload, SCOPE);
}

export async function bumpControlVersion(domain) {
  if (!isCloudflare()) return null;
  try {
    return await coordinatorCall("bumpVersion", { domain });
  } catch {
    return null;
  }
}

/**
 * Current version for `domain`, or the last known version when the coordinator
 * is unreachable. Throttled to one Durable Object call per domain per interval;
 * concurrent callers share the in-flight request instead of stampeding.
 */
export async function getControlVersion(domain) {
  if (!isCloudflare()) return null;

  const entry = domains.get(domain);
  if (entry?.pending) return await entry.pending;
  if (entry && Date.now() - entry.ts < POLL_INTERVAL_MS) return entry.version;

  const known = entry?.version ?? null;
  const pending = coordinatorCall("getVersion", { domain })
    .then((result) => {
      const version = result?.version ?? null;
      domains.set(domain, { version, ts: Date.now() });
      return version;
    })
    .catch(() => {
      // Keep serving the cached value and retry on the next interval.
      domains.set(domain, { version: known, ts: Date.now() });
      return known;
    });

  domains.set(domain, { version: known, ts: entry?.ts ?? 0, pending });
  return await pending;
}

export const __test__ = { POLL_INTERVAL_MS, reset: () => domains.clear() };
