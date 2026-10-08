/**
 * Durable Object contract tests for GatewayCoordinator.
 *
 * The DO is the cross-isolate enforcement surface (rate limits, concurrency
 * leases, budget reservations), so its logic is tested directly against a
 * faithful in-memory `ctx.storage` — not the client's memory fallback, which
 * only covers the non-Cloudflare path.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("cloudflare:workers", () => ({
  DurableObject: class {
    constructor(ctx, env) {
      this.ctx = ctx;
      this.env = env;
    }
  },
}));

const { GatewayCoordinator } = await import("@/cloudflare/gateway-coordinator.js");

function createStorage() {
  const data = new Map();
  return {
    data,
    async get(key) {
      return data.has(key) ? structuredClone(data.get(key)) : undefined;
    },
    async put(key, value) {
      data.set(key, structuredClone(value));
    },
    async delete(key) {
      data.delete(key);
    },
    async list() {
      return new Map([...data.entries()].map(([k, v]) => [k, structuredClone(v)]));
    },
  };
}

function createCoordinator() {
  const storage = createStorage();
  return {
    storage,
    do: new GatewayCoordinator({ storage }, {}),
  };
}

beforeEach(() => {
  vi.stubGlobal("crypto", globalThis.crypto ?? { randomUUID: () => "uuid" });
});

describe("GatewayCoordinator — rate limiting", () => {
  it("counts within a window and resets after it rolls over", async () => {
    const { do: coordinator } = createCoordinator();

    expect(await coordinator.checkRateLimit({ keyId: "k1", limit: 2, windowMs: 60_000, now: 1_000 }))
      .toMatchObject({ allowed: true, count: 1, remaining: 1, resetAt: 61_000 });
    expect(await coordinator.checkRateLimit({ keyId: "k1", limit: 2, windowMs: 60_000, now: 2_000 }))
      .toMatchObject({ allowed: true, count: 2, remaining: 0 });
    expect(await coordinator.checkRateLimit({ keyId: "k1", limit: 2, windowMs: 60_000, now: 3_000 }))
      .toMatchObject({ allowed: false, count: 3, remaining: 0, resetAt: 61_000 });

    // New window: the counter restarts instead of staying exhausted.
    expect(await coordinator.checkRateLimit({ keyId: "k1", limit: 2, windowMs: 60_000, now: 61_001 }))
      .toMatchObject({ allowed: true, count: 1, remaining: 1 });
  });

  it("keeps counters isolated per key", async () => {
    const { do: coordinator } = createCoordinator();
    await coordinator.checkRateLimit({ keyId: "k1", limit: 1, windowMs: 60_000, now: 1_000 });
    expect(await coordinator.checkRateLimit({ keyId: "k2", limit: 1, windowMs: 60_000, now: 1_000 }))
      .toMatchObject({ allowed: true, count: 1 });
  });

  it("peekRateLimit reports state without consuming budget", async () => {
    const { do: coordinator } = createCoordinator();
    expect(await coordinator.peekRateLimit({ keyId: "k1", limit: 2, windowMs: 60_000, now: 1_000 }))
      .toMatchObject({ count: 0, remaining: 2, resetAt: 61_000 });
    await coordinator.checkRateLimit({ keyId: "k1", limit: 2, windowMs: 60_000, now: 2_000 });
    // Peek twice: neither burns budget.
    expect(await coordinator.peekRateLimit({ keyId: "k1", limit: 2, windowMs: 60_000, now: 3_000 }))
      .toMatchObject({ count: 1, remaining: 1 });
    expect(await coordinator.peekRateLimit({ keyId: "k1", limit: 2, windowMs: 60_000, now: 4_000 }))
      .toMatchObject({ count: 1, remaining: 1 });
    expect(await coordinator.checkRateLimit({ keyId: "k1", limit: 2, windowMs: 60_000, now: 5_000 }))
      .toMatchObject({ allowed: true, count: 2, remaining: 0 });
  });
});

describe("GatewayCoordinator — concurrency leases", () => {
  it("denies beyond maxConcurrent and frees the slot on endRequest", async () => {
    const { do: coordinator } = createCoordinator();

    const first = await coordinator.beginRequest({ keyId: "k1", maxConcurrent: 2, now: 1_000 });
    const second = await coordinator.beginRequest({ keyId: "k1", maxConcurrent: 2, now: 1_000 });
    expect(first.allowed).toBe(true);
    expect(second.allowed).toBe(true);
    expect(first.leaseId).not.toBe(second.leaseId);

    expect(await coordinator.beginRequest({ keyId: "k1", maxConcurrent: 2, now: 1_000 }))
      .toMatchObject({ allowed: false, active: 2, leaseId: null });

    expect(await coordinator.endRequest({ keyId: "k1", leaseId: first.leaseId }))
      .toEqual({ active: 1 });
    expect(await coordinator.beginRequest({ keyId: "k1", maxConcurrent: 2, now: 1_000 }))
      .toMatchObject({ allowed: true, active: 2 });
  });

  it("reclaims leases abandoned by an isolate that died mid-stream", async () => {
    const { do: coordinator } = createCoordinator();
    await coordinator.beginRequest({ keyId: "k1", maxConcurrent: 1, now: 1_000 });

    // Lease TTL is 120s; the holder never called endRequest.
    expect(await coordinator.beginRequest({ keyId: "k1", maxConcurrent: 1, now: 1_000 }))
      .toMatchObject({ allowed: false });
    expect(await coordinator.beginRequest({ keyId: "k1", maxConcurrent: 1, now: 121_001 }))
      .toMatchObject({ allowed: true, active: 1 });
  });

  it("deletes the storage entry once the last lease ends", async () => {
    const { do: coordinator, storage } = createCoordinator();
    const lease = await coordinator.beginRequest({ keyId: "k1", maxConcurrent: 1, now: 1_000 });
    expect(storage.data.has("active:k1")).toBe(true);
    await coordinator.endRequest({ keyId: "k1", leaseId: lease.leaseId });
    expect(storage.data.has("active:k1")).toBe(false);
  });

  it("ignores an endRequest for an unknown lease id", async () => {
    const { do: coordinator } = createCoordinator();
    const lease = await coordinator.beginRequest({ keyId: "k1", maxConcurrent: 2, now: 1_000 });
    expect(await coordinator.endRequest({ keyId: "k1", leaseId: "not-a-lease" }))
      .toEqual({ active: 1 });
    expect(await coordinator.endRequest({ keyId: "k1", leaseId: lease.leaseId }))
      .toEqual({ active: 0 });
  });
});

describe("GatewayCoordinator — provider locks", () => {
  it("blocks a locked account for the requested model only", async () => {
    const { do: coordinator } = createCoordinator();
    await coordinator.setLock({ provider: "openai", connectionId: "acct-1", model: "gpt-5", untilMs: 5_000, status: 429, reason: "quota" });

    const locked = (await coordinator.getActiveLocks({ provider: "openai", connectionIds: ["acct-1"], model: "gpt-5", now: 2_000 })).locks;
    expect(locked).toHaveLength(1);
    expect(locked[0]).toMatchObject({ connectionId: "acct-1", untilMs: 5_000, status: 429, reason: "quota" });

    // A different model on the same account is unaffected.
    expect((await coordinator.getActiveLocks({ provider: "openai", connectionIds: ["acct-1"], model: "gpt-4", now: 2_000 })).locks).toHaveLength(0);
  });

  it("treats a model-less lock as account-wide and honours expiry", async () => {
    const { do: coordinator } = createCoordinator();
    await coordinator.setLock({ provider: "openai", connectionId: "acct-1", model: null, untilMs: 5_000, status: 401 });

    expect((await coordinator.getActiveLocks({ provider: "openai", connectionIds: ["acct-1"], model: "gpt-5", now: 2_000 })).locks).toHaveLength(1);
    expect((await coordinator.getActiveLocks({ provider: "openai", connectionIds: ["acct-1"], model: "gpt-5", now: 5_001 })).locks).toHaveLength(0);
  });

  it("clears a lock so the account becomes selectable again", async () => {
    const { do: coordinator } = createCoordinator();
    await coordinator.setLock({ provider: "openai", connectionId: "acct-1", model: "gpt-5", untilMs: 5_000 });
    expect(await coordinator.clearLock({ provider: "openai", connectionId: "acct-1", model: "gpt-5" }))
      .toEqual({ cleared: true });
    expect((await coordinator.getActiveLocks({ provider: "openai", connectionIds: ["acct-1"], model: "gpt-5", now: 2_000 })).locks).toHaveLength(0);
  });

  it("does not leak locks across providers sharing an account id", async () => {
    const { do: coordinator } = createCoordinator();
    await coordinator.setLock({ provider: "openai", connectionId: "shared-id", model: "m", untilMs: 5_000 });
    expect((await coordinator.getActiveLocks({ provider: "anthropic", connectionIds: ["shared-id"], model: "m", now: 2_000 })).locks).toHaveLength(0);
  });
});

describe("GatewayCoordinator — adaptive outcome scores", () => {
  it("accumulates samples, EWMA latency and consecutive failures", async () => {
    const { do: coordinator } = createCoordinator();

    await coordinator.recordOutcome({ provider: "openai", connectionId: "acct-1", model: "gpt-5", success: true, latencyMs: 100, status: 200, now: 1_000 });
    const second = await coordinator.recordOutcome({ provider: "openai", connectionId: "acct-1", model: "gpt-5", success: true, latencyMs: 200, status: 200, now: 1_100 });
    expect(second).toMatchObject({ samples: 2, successes: 2, failures: 0, consecutiveFailures: 0 });
    expect(second.ewmaLatencyMs).toBeCloseTo(130, 5);

    const failed = await coordinator.recordOutcome({ provider: "openai", connectionId: "acct-1", model: "gpt-5", success: false, latencyMs: 900, status: 500, now: 1_200 });
    expect(failed).toMatchObject({ samples: 3, failures: 1, consecutiveFailures: 1, lastStatus: 500 });

    // A success clears the breaker for that provider/model pair.
    const recovered = await coordinator.recordOutcome({ provider: "openai", connectionId: "acct-1", model: "gpt-5", success: true, latencyMs: 100, status: 200, now: 1_300 });
    expect(recovered.consecutiveFailures).toBe(0);
  });

  it("accumulates cost and never stores a negative amount", async () => {
    const { do: coordinator } = createCoordinator();
    await coordinator.recordOutcome({ provider: "openai", connectionId: "acct-1", model: "gpt-5", success: true, costUsd: 0.25, now: 1_000 });
    const entry = await coordinator.recordOutcome({ provider: "openai", connectionId: "acct-1", model: "gpt-5", success: true, costUsd: -5, now: 1_100 });
    expect(entry.totalCostUsd).toBeCloseTo(0.25, 5);
  });

  it("keeps scores per model and drops entries older than 24h", async () => {
    const { do: coordinator } = createCoordinator();
    await coordinator.recordOutcome({ provider: "openai", connectionId: "acct-1", model: "gpt-5", success: true, latencyMs: 100, status: 200, now: 1_000 });
    await coordinator.recordOutcome({ provider: "openai", connectionId: "acct-1", model: "gpt-4", success: true, latencyMs: 100, status: 200, now: 1_000 });

    const fresh = await coordinator.getScores({ provider: "openai", connectionIds: ["acct-1"], model: "gpt-5", now: 2_000 });
    expect(fresh).toHaveLength(1);

    const stale = await coordinator.getScores({ provider: "openai", connectionIds: ["acct-1"], model: "gpt-5", now: 1_000 + 86_400_001 });
    expect(stale).toHaveLength(0);
  });

  it("omits accounts that were never observed", async () => {
    const { do: coordinator } = createCoordinator();
    await coordinator.recordOutcome({ provider: "openai", connectionId: "acct-1", model: "gpt-5", success: true, latencyMs: 100, status: 200, now: 1_000 });
    const scores = await coordinator.getScores({ provider: "openai", connectionIds: ["acct-1", "acct-2"], model: "gpt-5", now: 2_000 });
    expect(scores.map((entry) => entry.connectionId)).toEqual(["acct-1"]);
  });
});

describe("GatewayCoordinator — budget reservations", () => {
  it("rejects a reservation that would exceed the available budget", async () => {
    const { do: coordinator } = createCoordinator();

    expect(await coordinator.reserveBudget({ keyId: "k1", requestId: "r1", amount: 6, availableBudget: 10, expiresAt: 5_000, now: 1_000 }))
      .toMatchObject({ reserved: true, amount: 6 });
    expect(await coordinator.reserveBudget({ keyId: "k1", requestId: "r2", amount: 5, availableBudget: 10, expiresAt: 5_000, now: 1_001 }))
      .toMatchObject({ reserved: false, reservedAmount: 6 });
  });

  it("is idempotent for the same requestId", async () => {
    const { do: coordinator } = createCoordinator();
    await coordinator.reserveBudget({ keyId: "k1", requestId: "r1", amount: 6, availableBudget: 10, expiresAt: 5_000, now: 1_000 });
    const retry = await coordinator.reserveBudget({ keyId: "k1", requestId: "r1", amount: 6, availableBudget: 10, expiresAt: 5_000, now: 1_010 });
    expect(retry).toMatchObject({ reserved: true, amount: 6 });
    expect(await coordinator.getBudgetReservations({ keyId: "k1", now: 1_011 })).toHaveLength(1);
  });

  it("frees the budget again after releaseBudget", async () => {
    const { do: coordinator } = createCoordinator();
    await coordinator.reserveBudget({ keyId: "k1", requestId: "r1", amount: 6, availableBudget: 10, expiresAt: 5_000, now: 1_000 });
    await coordinator.releaseBudget({ keyId: "k1", requestId: "r1", now: 1_100 });
    expect(await coordinator.getBudgetReservations({ keyId: "k1", now: 1_100 })).toHaveLength(0);
    expect(await coordinator.reserveBudget({ keyId: "k1", requestId: "r2", amount: 10, availableBudget: 10, expiresAt: 5_000, now: 1_101 }))
      .toMatchObject({ reserved: true });
  });

  it("ignores expired reservations so a crashed request cannot wedge the budget", async () => {
    const { do: coordinator } = createCoordinator();
    await coordinator.reserveBudget({ keyId: "k1", requestId: "r1", amount: 10, availableBudget: 10, expiresAt: 2_000, now: 1_000 });
    expect(await coordinator.getBudgetReservations({ keyId: "k1", now: 1_500 })).toHaveLength(1);
    expect(await coordinator.getBudgetReservations({ keyId: "k1", now: 2_001 })).toHaveLength(0);
    expect(await coordinator.reserveBudget({ keyId: "k1", requestId: "r2", amount: 10, availableBudget: 10, expiresAt: 9_000, now: 2_002 }))
      .toMatchObject({ reserved: true });
  });

  it("keeps reservations isolated per key", async () => {
    const { do: coordinator } = createCoordinator();
    await coordinator.reserveBudget({ keyId: "k1", requestId: "r1", amount: 10, availableBudget: 10, expiresAt: 5_000, now: 1_000 });
    expect(await coordinator.reserveBudget({ keyId: "k2", requestId: "r2", amount: 10, availableBudget: 10, expiresAt: 5_000, now: 1_000 }))
      .toMatchObject({ reserved: true });
  });
});

describe("GatewayCoordinator — snapshot", () => {
  it("returns every stored key/value pair", async () => {
    const { do: coordinator } = createCoordinator();
    await coordinator.checkRateLimit({ keyId: "k1", limit: 5, windowMs: 1_000, now: 1_000 });
    await coordinator.setLock({ provider: "openai", connectionId: "acct-1", model: "gpt-5", untilMs: 9_000 });

    const snapshot = await coordinator.snapshot();
    const keys = snapshot.map((entry) => entry.key);
    expect(keys).toContain("rate:k1");
    expect(keys).toContain("lock:openai:acct-1:gpt-5");
    expect(snapshot.find((entry) => entry.key === "rate:k1").value).toMatchObject({ count: 1 });
  });

  it("does not throw on an empty store", async () => {
    const { do: coordinator } = createCoordinator();
    expect(await coordinator.snapshot()).toEqual([]);
  });
});

describe("GatewayCoordinator — durable metrics", () => {
  it("aggregates outcomes from every isolate into one cluster-wide view", async () => {
    const { do: coordinator } = createCoordinator();

    await coordinator.recordMetrics({ outcome: { provider: "openai", connectionId: "a1", model: "gpt-5", success: true, status: 200, latencyMs: 100, now: 1_000 } });
    await coordinator.recordMetrics({ outcome: { provider: "openai", connectionId: "a1", model: "gpt-5", success: true, status: 200, latencyMs: 200, now: 1_100 } });
    await coordinator.recordMetrics({ outcome: { provider: "openai", connectionId: "a2", model: "gpt-5", success: false, status: 500, latencyMs: 400, now: 1_200 } });

    const metrics = await coordinator.getMetrics();
    expect(metrics.providers).toHaveLength(2);
    expect(metrics.counters.find((c) => c.name === "provider_requests_total").value).toBe(3);
    expect(metrics.latency).toMatchObject({ count: 3, sumMs: 700, maxMs: 400 });
    expect(metrics.latency.averageMs).toBeCloseTo(700 / 3, 5);
  });

  it("derives alerts from the aggregated view, not per-isolate state", async () => {
    const { do: coordinator } = createCoordinator();
    for (let i = 0; i < 3; i++) {
      await coordinator.recordMetrics({ outcome: { provider: "anthropic", connectionId: "z9", model: "claude", success: false, status: 503, latencyMs: 50, now: 1_000 + i } });
    }

    const metrics = await coordinator.getMetrics();
    expect(metrics.alerts).toHaveLength(1);
    expect(metrics.alerts[0]).toMatchObject({ severity: "warning", provider: "anthropic", connectionId: "z9" });
  });

  it("clears the alert after a success", async () => {
    const { do: coordinator } = createCoordinator();
    for (let i = 0; i < 3; i++) {
      await coordinator.recordMetrics({ outcome: { provider: "anthropic", connectionId: "z9", model: "claude", success: false, status: 503, latencyMs: 50, now: 1_000 + i } });
    }
    await coordinator.recordMetrics({ outcome: { provider: "anthropic", connectionId: "z9", model: "claude", success: true, status: 200, latencyMs: 50, now: 2_000 } });

    expect((await coordinator.getMetrics()).alerts).toHaveLength(0);
  });

  it("returns an empty but well-formed view before any traffic", async () => {
    const { do: coordinator } = createCoordinator();
    const metrics = await coordinator.getMetrics();
    expect(metrics.providers).toEqual([]);
    expect(metrics.alerts).toEqual([]);
    expect(metrics.latency).toMatchObject({ count: 0, averageMs: 0 });
  });

  it("ignores a malformed outcome payload", async () => {
    const { do: coordinator } = createCoordinator();
    expect(await coordinator.recordMetrics({ outcome: {} })).toEqual({ recorded: false });
    expect((await coordinator.getMetrics()).providers).toEqual([]);
  });
});

describe("GatewayCoordinator — lease renewal", () => {
  it("extends a lease so a long stream keeps its slot", async () => {
    const { do: coordinator } = createCoordinator();
    const lease = await coordinator.beginRequest({ keyId: "k1", maxConcurrent: 1, now: 1_000 });
    expect(lease.leaseId).toBeTruthy();

    // Renew partway through, well before the original 120s expiry.
    expect(await coordinator.renewRequest({ keyId: "k1", leaseId: lease.leaseId, ttlMs: 120_000, now: 60_000 }))
      .toMatchObject({ renewed: true, active: 1 });

    // Past the original expiry the slot is still held, because it was renewed.
    expect(await coordinator.beginRequest({ keyId: "k1", maxConcurrent: 1, now: 121_001 }))
      .toMatchObject({ allowed: false, active: 1 });

    // Past the renewed expiry it finally lapses.
    expect(await coordinator.beginRequest({ keyId: "k1", maxConcurrent: 1, now: 180_002 }))
      .toMatchObject({ allowed: true });
  });

  it("reports an unknown lease instead of creating one", async () => {
    const { do: coordinator } = createCoordinator();
    await coordinator.beginRequest({ keyId: "k1", maxConcurrent: 2, now: 1_000 });
    expect(await coordinator.renewRequest({ keyId: "k1", leaseId: "ghost", now: 2_000 }))
      .toEqual({ renewed: false, active: 1 });
  });

  it("renews one lease without disturbing the others", async () => {
    const { do: coordinator } = createCoordinator();
    const a = await coordinator.beginRequest({ keyId: "k1", maxConcurrent: 3, now: 1_000 });
    const b = await coordinator.beginRequest({ keyId: "k1", maxConcurrent: 3, now: 1_000 });

    await coordinator.renewRequest({ keyId: "k1", leaseId: a.leaseId, ttlMs: 300_000, now: 50_000 });

    // b still carries the original expiry, so it lapsed at 121s and is no
    // longer counted as active...
    expect(await coordinator.renewRequest({ keyId: "k1", leaseId: b.leaseId, now: 200_000 }))
      .toEqual({ renewed: false, active: 1 });
    // ...while a, renewed to 350s, is still there and still renewable.
    expect(await coordinator.renewRequest({ keyId: "k1", leaseId: a.leaseId, now: 200_000 }))
      .toMatchObject({ renewed: true, active: 1 });
  });
});

describe("GatewayCoordinator — lock generations", () => {
  it("stamps locks with a monotonic generation per provider", async () => {
    const { do: coordinator } = createCoordinator();
    const first = await coordinator.setLock({ provider: "openai", connectionId: "a", model: "m", untilMs: 9_000, now: 1_000 });
    const second = await coordinator.setLock({ provider: "openai", connectionId: "b", model: "m", untilMs: 9_000, now: 1_001 });
    expect(second.generation).toBeGreaterThan(first.generation);

    const other = await coordinator.setLock({ provider: "anthropic", connectionId: "a", model: "m", untilMs: 9_000, now: 1_002 });
    expect(other.generation).toBe(1);
  });

  it("refuses to clear a lock created after the caller observed the state", async () => {
    const { do: coordinator } = createCoordinator();

    // A request selects the account while nothing is locked.
    const observed = await coordinator.getActiveLocks({ provider: "openai", connectionIds: ["a"], model: "m", now: 1_000 });
    expect(observed.locks).toHaveLength(0);

    // Meanwhile another request fails and locks it.
    await coordinator.setLock({ provider: "openai", connectionId: "a", model: "m", untilMs: 60_000, status: 429, now: 1_500 });

    // The first request now succeeds; its stale view must not clear the lock.
    const result = await coordinator.clearLock({ provider: "openai", connectionId: "a", model: "m", expectedGeneration: observed.generation, now: 2_000 });
    expect(result).toMatchObject({ cleared: false, reason: "newer_lock" });
    expect((await coordinator.getActiveLocks({ provider: "openai", connectionIds: ["a"], model: "m", now: 2_000 })).locks).toHaveLength(1);
  });

  it("allows a caller that observed the current generation to clear it", async () => {
    const { do: coordinator } = createCoordinator();
    await coordinator.setLock({ provider: "openai", connectionId: "a", model: "m", untilMs: 60_000, now: 1_000 });
    const observed = await coordinator.getActiveLocks({ provider: "openai", connectionIds: ["a"], model: "m", now: 2_000 });
    expect(observed.locks).toHaveLength(1);

    expect(await coordinator.clearLock({ provider: "openai", connectionId: "a", model: "m", expectedGeneration: observed.generation, now: 2_100 }))
      .toEqual({ cleared: true });
    expect((await coordinator.getActiveLocks({ provider: "openai", connectionIds: ["a"], model: "m", now: 2_200 })).locks).toHaveLength(0);
  });

  it("keeps unconditional clears working for callers with no observation", async () => {
    const { do: coordinator } = createCoordinator();
    await coordinator.setLock({ provider: "openai", connectionId: "a", model: "m", untilMs: 60_000, now: 1_000 });
    expect(await coordinator.clearLock({ provider: "openai", connectionId: "a", model: "m", now: 2_000 }))
      .toEqual({ cleared: true });
  });
});
