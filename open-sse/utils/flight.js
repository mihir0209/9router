// Request-safe sharing of in-flight work.
//
// Why this exists: on Cloudflare Workers a promise that was started by one request
// depends on that request's I/O context. When the owner is cancelled (client
// disconnect, CPU limit kill, 1102) its pending I/O is dropped and the promise
// NEVER settles. A module-level `Map<key, Promise>` that other requests await then
// hangs every follower, and because the entry is only removed in a `finally` that
// never runs, it keeps hanging every later request too (observed: GET /v1/models
// requests hung for ~330 s). Timers have the same property: a setTimeout created by
// the cancelled owner never fires, so a timeout armed by the owner cannot rescue a
// follower. Followers must arm their OWN timer.
//
// Rules implemented here:
//   * a follower waits for the leader at most `followerWaitMs`, measured by a timer
//     the follower creates in its own context;
//   * if the leader is slower than that, rejects, or the entry is older than
//     `staleMs`, the follower does its own work (own I/O) instead of waiting;
//   * an entry never outlives `staleMs`, so a dead leader cannot poison later calls;
//   * the leader's result is shared only while it is in flight; nothing is cached.

export class FlightTimeoutError extends Error {
  constructor(label, ms) {
    super(`${label} timed out after ${ms}ms`);
    this.name = "FlightTimeoutError";
    this.code = "FLIGHT_TIMEOUT";
  }
}

/**
 * Race `promise` against a timer armed NOW, in the caller's own request context.
 * Rejects with FlightTimeoutError. The timer is always cleared.
 */
export function withOwnTimeout(promise, ms, label = "operation") {
  if (!(ms > 0)) return Promise.resolve(promise);
  let timer;
  const guard = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new FlightTimeoutError(label, ms)), ms);
  });
  return Promise.race([promise, guard]).finally(() => clearTimeout(timer));
}

// NINEROUTER_FLIGHT_WAIT_MS overrides every group's follower wait (tests, tuning).
function envWaitOverride() {
  const raw = typeof process !== "undefined" ? process.env?.NINEROUTER_FLIGHT_WAIT_MS : undefined;
  if (raw === undefined || raw === "") return null;
  const n = Number(raw);
  return Number.isFinite(n) && n >= 0 ? n : null;
}

/**
 * A group of keyed flights.
 *
 * `run(key, factory)` returns `{ value, shared }`. `factory` is called by the
 * leader (or by a follower that gave up waiting) and must perform its own I/O.
 */
export function createFlightGroup({ followerWaitMs = 2000, staleMs = 15000, now = () => Date.now() } = {}) {
  const flights = new Map(); // key -> { promise, startedAt }

  function start(key, factory) {
    const promise = Promise.resolve().then(factory);
    const flight = { promise, startedAt: now() };
    flights.set(key, flight);
    const clear = () => { if (flights.get(key) === flight) flights.delete(key); };
    promise.then(clear, clear); // detached: a rejection is surfaced to the awaiting caller only
    return promise;
  }

  async function run(key, factory, opts = {}) {
    const waitMs = envWaitOverride() ?? opts.followerWaitMs ?? followerWaitMs;
    const maxAge = opts.staleMs ?? staleMs;
    const current = flights.get(key);
    if (current && now() - current.startedAt < maxAge) {
      try {
        return { value: await withOwnTimeout(current.promise, waitMs, `shared ${key}`), shared: true };
      } catch {
        // Leader failed or is too slow (possibly dead): do the work ourselves.
        const own = Promise.resolve().then(factory);
        if (flights.get(key) === current) {
          const flight = { promise: own, startedAt: now() };
          flights.set(key, flight);
          const clear = () => { if (flights.get(key) === flight) flights.delete(key); };
          own.then(clear, clear);
        }
        return { value: await own, shared: false };
      }
    }
    return { value: await start(key, factory), shared: false };
  }

  return {
    run,
    size: () => flights.size,
    clear: () => flights.clear(),
  };
}
