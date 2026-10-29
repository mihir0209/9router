import { createFlightGroup } from "../../utils/flight.js";

const REFRESH_RESULT_TTL_MS = 10_000;
const refreshDedupCache = new Map(); // key -> { result, expiresAt } (completed results only)

// In-flight refreshes are shared through a request-safe flight: a refresh started by a request that was
// cancelled (Workers) never settles, and a bare shared promise would block every later refresh of the
// same token. The follower waits on a timer of its own (long enough for a slow OAuth endpoint).
const flights = createFlightGroup({ followerWaitMs: 15000, staleMs: 30000 });

export async function dedupRefresh(provider, oldToken, fn, log) {
  if (!oldToken) return fn();
  const key = `${provider}:${oldToken}`;
  const hit = refreshDedupCache.get(key);
  if (hit) {
    if (hit.expiresAt > Date.now()) {
      log?.info?.("TOKEN_REFRESH", `Reusing recent refresh result for ${provider}`);
      return hit.result;
    }
    refreshDedupCache.delete(key);
  }
  const { value, shared } = await flights.run(key, async () => {
    const result = await fn();
    refreshDedupCache.set(key, { result, expiresAt: Date.now() + REFRESH_RESULT_TTL_MS });
    return result;
  });
  if (shared) log?.info?.("TOKEN_REFRESH", `Reused in-flight refresh for ${provider}`);
  return value;
}
