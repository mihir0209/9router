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
 * is one subrequest every poll interval (pollIntervalMs) — not one per request.
 */

import { isExtremeProfile, extremeTunables } from "@/lib/db/writeProfile.js";
import { createFlightGroup } from "open-sse/utils/flight.js";

const POLL_INTERVAL_MS = 2_000;
const SCOPE = "control";

// The extreme profile polls less often: each poll is a billed Durable Object
// request (and storage read) per domain per isolate, and under sparse traffic
// every request lands on an expired interval. The price is a longer window
// before another isolate notices a key revocation / kill switch (the isolate
// that performs the write invalidates itself immediately).
export function pollIntervalMs() {
  return isExtremeProfile() ? extremeTunables().controlPollMs : POLL_INTERVAL_MS;
}

const domains = new Map();

// Concurrent callers share one coordinator call only through a request-safe flight:
// a promise owned by a cancelled Workers request never settles, and the old shared
// `pending` entry then hung every authenticated request of the isolate.
// The wait is bounded by a timer the FOLLOWER owns (open-sse/utils/flight.js).
const flights = createFlightGroup({ followerWaitMs: 2000, staleMs: 10000 });

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
 * concurrent callers share the in-flight request (bounded, request-safe) instead of stampeding.
 */
export async function getControlVersion(domain) {
  if (!isCloudflare()) return null;

  const entry = domains.get(domain);
  if (entry && Date.now() - entry.ts < pollIntervalMs()) return entry.version;

  // Extreme profile: refresh every known domain with ONE coordinator call
  // (getVersions) instead of one call per domain. Falls back to the
  // single-domain call if the coordinator predates getVersions.
  if (isExtremeProfile()) return await pollDomains(domain);

  const known = entry?.version ?? null;
  const { value } = await flights.run(`v:${domain}`, async () => {
    try {
      const result = await coordinatorCall("getVersion", { domain });
      const version = result?.version ?? null;
      domains.set(domain, { version, ts: Date.now() });
      return version;
    } catch {
      // Keep serving the cached value and retry on the next interval.
      domains.set(domain, { version: known, ts: Date.now() });
      return known;
    }
  });
  return value;
}

async function pollDomains(requested) {
  const names = [...new Set([...domains.keys(), requested])];
  const known = new Map(names.map((name) => [name, domains.get(name)?.version ?? null]));
  const { value } = await flights.run(`poll:${requested}`, async () => {
    try {
      const result = await coordinatorCall("getVersions", { domains: names });
      const versions = result?.versions || {};
      for (const name of names) domains.set(name, { version: versions[name] ?? known.get(name), ts: Date.now() });
    } catch {
      try {
        const single = await coordinatorCall("getVersion", { domain: requested });
        domains.set(requested, { version: single?.version ?? known.get(requested), ts: Date.now() });
      } catch {
        for (const name of names) domains.set(name, { version: known.get(name), ts: Date.now() });
      }
    }
    return domains.get(requested)?.version ?? null;
  });
  return value;
}

export const __test__ = { POLL_INTERVAL_MS, pollIntervalMs, reset: () => { domains.clear(); flights.clear(); } };
