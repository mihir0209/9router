// Request-path traffic shaping for the coordinator Durable Object.
//
// Every proxied request used to cost the Durable Object (SQLite-backed, billed
// per request and per row read/written) about 3 RPCs, 7 row reads and 4 row
// writes: getActiveLocks, recordOutcome and recordMetrics. This module turns
// that into amortised work, without changing what the coordinator ends up
// knowing:
//
//   * reads (locks, scores) are served from a short TTL cache;
//   * outcome and metric reports are aggregated in memory and sent as one batch
//     per provider scope every `flushMs` (or `maxEvents` events), with the
//     batch sent through ctx.waitUntil when a Workers context is available.
//
// Trade-offs, all explicit:
//   * a lock another isolate sets is seen after at most `readTtlMs`; this
//     isolate invalidates its own cache the moment it sets or clears one.
//   * events not yet flushed when an isolate is evicted are lost (the same
//     exposure the analytics buffer already has). They feed adaptive scoring
//     and observability only, never enforcement.
//
import { createFlightGroup, withOwnTimeout } from "../../../open-sse/utils/flight.js";

// The module is runtime-agnostic: `call(method, payload, scope)` is injected,
// and `waitUntil` is optional.

const FLUSH_STALE_MS = 15_000;
const FLUSH_JOIN_MS = 3_000;

export function createCoalescer({ call, now = () => Date.now(), tunables, waitUntil = null, onError = () => {} }) {
  const cache = new Map(); // key -> { value, expires, scope }
  const reads = createFlightGroup({ followerWaitMs: 1500, staleMs: 8000 });
  let flushingSince = 0;
  const CACHE_CAP = 1000;

  // pending aggregates
  const outcomes = new Map(); // `${provider}|${connectionId}|${model}` -> item
  const metrics = new Map(); // `${tenant}|${provider}|${connectionId}|${model}` -> item
  let pendingEvents = 0;
  let firstEventAt = null;
  let timer = null;
  let flushing = null;

  const config = () => (typeof tunables === "function" ? tunables() : tunables);

  // ---------- cached reads ----------

  async function read(key, scope, loader) {
    const ttl = config().coordinatorReadTtlMs;
    if (!(ttl > 0)) return loader();
    const hit = cache.get(key);
    const t = now();
    if (hit && hit.expires > t) return hit.value;
    // Request-safe sharing: a follower waits on a timer of its own, then loads itself
    // (a read started by a cancelled Workers request never settles).
    const { value } = await reads.run(key, async () => {
      const loaded = await loader();
      if (cache.size >= CACHE_CAP) cache.delete(cache.keys().next().value);
      cache.set(key, { value: loaded, expires: now() + ttl, scope });
      return loaded;
    });
    return value;
  }

  /** Drop cached reads for a coordinator scope (after this isolate changed that state). */
  function invalidate(scope) {
    for (const [key, entry] of cache) if (entry.scope === scope) cache.delete(key);
  }

  // ---------- coalesced writes ----------

  function noteEvent() {
    pendingEvents += 1;
    const t = now();
    if (firstEventAt === null) firstEventAt = t;
    const { coordinatorFlushMs, coordinatorFlushMaxEvents } = config();
    if (pendingEvents >= coordinatorFlushMaxEvents || t - firstEventAt >= coordinatorFlushMs) {
      const task = flush();
      if (waitUntil) waitUntil(task);
      return;
    }
    if (!timer) {
      timer = setTimeout(() => {
        timer = null;
        const task = flush();
        if (waitUntil) waitUntil(task);
      }, Math.max(1, coordinatorFlushMs - (t - firstEventAt)));
      timer.unref?.();
    }
  }

  function queueOutcome({ provider, connectionId, model, success, latencyMs, status, costUsd = 0 }) {
    if (!provider || !connectionId) return;
    const key = `${provider}|${connectionId}|${model}`;
    const item = outcomes.get(key) || {
      provider, connectionId, model, samples: 0, successes: 0, failures: 0, tailFailures: 0,
      latencySumMs: 0, latencyCount: 0, costUsd: 0, lastStatus: 0,
    };
    item.samples += 1;
    if (success) { item.successes += 1; item.tailFailures = 0; } else { item.failures += 1; item.tailFailures += 1; }
    if (Number.isFinite(latencyMs)) { item.latencySumMs += Math.max(latencyMs, 0); item.latencyCount += 1; }
    item.costUsd += Math.max(Number(costUsd) || 0, 0);
    item.lastStatus = Number(status) || 0;
    outcomes.set(key, item);
    noteEvent();
  }

  function queueMetrics({ tenant, provider, connectionId, model, success, status, latencyMs }) {
    if (!provider) return;
    const key = `${tenant || "anonymous"}|${provider}|${connectionId}|${model}`;
    const item = metrics.get(key) || {
      tenant: tenant || "anonymous", provider, connectionId, model, successes: 0, failures: 0, tailFailures: 0,
      lastStatus: 0, lastLatencyMs: null, latencyCount: 0, latencySumMs: 0, latencyMaxMs: 0,
    };
    if (success) { item.successes += 1; item.tailFailures = 0; } else { item.failures += 1; item.tailFailures += 1; }
    item.lastStatus = Number(status) || 0;
    if (Number.isFinite(latencyMs)) {
      const l = Math.max(latencyMs, 0);
      item.lastLatencyMs = l;
      item.latencyCount += 1;
      item.latencySumMs += l;
      item.latencyMaxMs = Math.max(item.latencyMaxMs, l);
    }
    metrics.set(key, item);
    noteEvent();
  }

  /** Send everything pending. Safe to call concurrently; failures requeue nothing (best effort). */
  async function flush() {
    if (flushing && now() - flushingSince < FLUSH_STALE_MS) {
      // A flush started by a cancelled request never settles; do not wait on it forever.
      try { return await withOwnTimeout(flushing, FLUSH_JOIN_MS, "coordinator flush"); } catch { return { sent: 0, joined: false }; }
    }
    if (timer) { clearTimeout(timer); timer = null; }
    if (!outcomes.size && !metrics.size) { pendingEvents = 0; firstEventAt = null; return { sent: 0 }; }
    const outcomeItems = [...outcomes.values()];
    const metricItems = [...metrics.values()];
    outcomes.clear();
    metrics.clear();
    const sentEvents = pendingEvents;
    pendingEvents = 0;
    firstEventAt = null;

    flushingSince = now();
    const current = (async () => {
      const byProvider = new Map();
      for (const item of outcomeItems) {
        if (!byProvider.has(item.provider)) byProvider.set(item.provider, []);
        byProvider.get(item.provider).push(item);
      }
      const calls = [];
      for (const [provider, items] of byProvider) {
        calls.push(call("recordOutcomes", { items, now: now() }, `provider:${provider}`));
      }
      if (metricItems.length) calls.push(call("recordMetricsBatch", { items: metricItems, now: now() }, "metrics"));
      const results = await Promise.allSettled(calls);
      for (const r of results) if (r.status === "rejected") onError(r.reason);
      return { sent: sentEvents, rpcs: calls.length };
    })().finally(() => { if (flushing === current) flushing = null; });
    flushing = current;
    return current;
  }

  return {
    read,
    invalidate,
    queueOutcome,
    queueMetrics,
    flush,
    pending: () => ({ events: pendingEvents, outcomeKeys: outcomes.size, metricKeys: metrics.size }),
    clear() { cache.clear(); outcomes.clear(); metrics.clear(); pendingEvents = 0; firstEventAt = null; if (timer) { clearTimeout(timer); timer = null; } },
  };
}
