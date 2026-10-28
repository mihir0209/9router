// Extreme profile against the REAL GatewayCoordinator class: batched reports
// must leave the Durable Object in the same state as the per-request calls.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const holder = vi.hoisted(() => ({ counter: null }));
vi.mock("cloudflare:workers", () => ({ DurableObject: class { constructor(ctx, env) { this.ctx = ctx; this.env = env; } } }));
vi.mock("@opennextjs/cloudflare", () => ({
  getCloudflareContext: async () => ({ env: { GATEWAY_COORDINATOR: holder.counter?.namespace }, ctx: { waitUntil: () => {} } }),
}));

const { createCountingDO } = await import("../helpers/countingDO.js");

beforeEach(() => {
  process.env.NINEROUTER_RUNTIME = "cloudflare";
  delete process.env.NINEROUTER_D1_PROFILE;
  delete global._coordinatorCoalescer;
  vi.resetModules();
});
afterEach(() => {
  delete process.env.NINEROUTER_RUNTIME;
  delete process.env.NINEROUTER_D1_PROFILE;
  delete global._coordinatorCoalescer;
});

async function scoreOf(client, provider, connectionId, model) {
  const [entry] = await client.coordinatorCall("getScores", { provider, model, connectionIds: [connectionId] }, `provider:${provider}`);
  return entry;
}

const events = [
  { success: true, latencyMs: 100, status: 200, costUsd: 0.01 },
  { success: true, latencyMs: 100, status: 200, costUsd: 0.01 },
  { success: false, latencyMs: 100, status: 500, costUsd: 0 },
  { success: false, latencyMs: 100, status: 429, costUsd: 0 },
  { success: true, latencyMs: 100, status: 200, costUsd: 0.02 },
  { success: false, latencyMs: 100, status: 503, costUsd: 0 },
];

describe("batched outcome reports are equivalent to per-request reports", () => {
  it("adaptive-score state matches (counts, trailing failures, cost, status; constant-latency EWMA)", async () => {
    // Standard: one recordOutcome per event.
    holder.counter = await createCountingDO();
    process.env.NINEROUTER_D1_PROFILE = "standard";
    let client = await import("@/lib/coordinator/client.js");
    for (const e of events) await client.recordProviderOutcome({ provider: "p", connectionId: "c1", model: "m", ...e });
    const standard = await scoreOf(client, "p", "c1", "m");
    const standardCost = holder.counter.snapshot();

    // Extreme: queued, then one batched recordOutcomes.
    holder.counter = await createCountingDO();
    process.env.NINEROUTER_D1_PROFILE = "extreme";
    delete global._coordinatorCoalescer;
    vi.resetModules();
    client = await import("@/lib/coordinator/client.js");
    for (const e of events) await client.recordProviderOutcome({ provider: "p", connectionId: "c1", model: "m", ...e });
    expect(holder.counter.snapshot().rpcs).toBe(0); // nothing sent yet
    await client.flushCoordinatorBatches();
    const extreme = await scoreOf(client, "p", "c1", "m");

    for (const field of ["samples", "successes", "failures", "consecutiveFailures", "lastStatus", "provider", "connectionId", "model"]) {
      expect(extreme[field], field).toEqual(standard[field]);
    }
    expect(extreme.totalCostUsd).toBeCloseTo(standard.totalCostUsd, 10);
    expect(extreme.ewmaLatencyMs).toBeCloseTo(standard.ewmaLatencyMs, 6);
    // 6 requests: standard wrote once per request, extreme once for the batch.
    expect(standardCost.rowsWritten).toBe(6);
    expect(holder.counter.snapshot().rowsWritten).toBe(1);
  });

  it("a second batch accumulates on top of the stored score", async () => {
    holder.counter = await createCountingDO();
    process.env.NINEROUTER_D1_PROFILE = "extreme";
    const client = await import("@/lib/coordinator/client.js");
    for (const e of events.slice(0, 3)) await client.recordProviderOutcome({ provider: "p", connectionId: "c1", model: "m", ...e });
    await client.flushCoordinatorBatches();
    for (const e of events.slice(3)) await client.recordProviderOutcome({ provider: "p", connectionId: "c1", model: "m", ...e });
    await client.flushCoordinatorBatches();
    const entry = await scoreOf(client, "p", "c1", "m");
    expect(entry).toMatchObject({ samples: 6, successes: 3, failures: 3, consecutiveFailures: 1, lastStatus: 503 });
  });

  it("metrics totals and per-tenant rollups match", async () => {
    const outcome = (e, tenant) => ({ tenant, provider: "p", connectionId: "c1", model: "m", success: e.success, status: e.status, latencyMs: e.latencyMs });

    holder.counter = await createCountingDO();
    process.env.NINEROUTER_D1_PROFILE = "standard";
    let client = await import("@/lib/coordinator/client.js");
    for (const e of events) await client.coordinatorCall("recordMetrics", { outcome: outcome(e, "acme") }, "metrics");
    const standard = await client.coordinatorCall("getMetrics", { tenant: "acme" }, "metrics");
    const standardAll = await client.coordinatorCall("getMetrics", {}, "metrics");
    const standardWrites = holder.counter.snapshot();

    holder.counter = await createCountingDO();
    process.env.NINEROUTER_D1_PROFILE = "extreme";
    delete global._coordinatorCoalescer;
    vi.resetModules();
    client = await import("@/lib/coordinator/client.js");
    for (const e of events) await client.recordMetricsOutcome(outcome(e, "acme"));
    await client.flushCoordinatorBatches();
    const extreme = await client.coordinatorCall("getMetrics", { tenant: "acme" }, "metrics");
    const extremeAll = await client.coordinatorCall("getMetrics", {}, "metrics");

    expect(extreme.counters).toEqual(standard.counters);
    expect(extremeAll.counters).toEqual(standardAll.counters);
    expect(extremeAll.latency).toMatchObject({ count: 6, sumMs: 600, maxMs: 100 });
    expect(extreme.providers[0]).toMatchObject({ successes: 3, failures: 3, consecutiveFailures: 1, lastStatus: 503 });
    expect(extremeAll.alerts.length).toBe(standardAll.alerts.length);
    expect(standardWrites.rowsWritten).toBe(18); // 3 puts x 6 events
    expect(holder.counter.snapshot().rowsWritten).toBe(3 + 0); // outcome row + totals + tenant rollup
  });
});

describe("coordinator reads", () => {
  it("getActiveLocks is cached; a local setLock invalidates it immediately", async () => {
    holder.counter = await createCountingDO();
    process.env.NINEROUTER_D1_PROFILE = "extreme";
    const client = await import("@/lib/coordinator/client.js");
    const args = { provider: "p", connectionIds: ["c1"], model: "m" };
    const read = () => client.coordinatorRead("getActiveLocks", args, "provider:p");

    expect((await read()).locks).toEqual([]);
    await read(); await read();
    expect(holder.counter.snapshot().byMethod.getActiveLocks).toBe(1);

    await client.coordinatorCall("setLock", { provider: "p", connectionId: "c1", model: "m", untilMs: Date.now() + 60_000, status: 429, reason: "x" }, "provider:p");
    const after = await read();
    expect(after.locks.map((l) => l.connectionId)).toEqual(["c1"]);
    expect(holder.counter.snapshot().byMethod.getActiveLocks).toBe(2);
  });

  it("the standard profile does not cache", async () => {
    holder.counter = await createCountingDO();
    process.env.NINEROUTER_D1_PROFILE = "standard";
    const client = await import("@/lib/coordinator/client.js");
    const args = { provider: "p", connectionIds: ["c1"], model: "m" };
    await client.coordinatorRead("getActiveLocks", args, "provider:p");
    await client.coordinatorRead("getActiveLocks", args, "provider:p");
    expect(holder.counter.snapshot().byMethod.getActiveLocks).toBe(2);
  });
});

describe("control version polling", () => {
  it("extreme polls every known domain with one getVersions call", async () => {
    holder.counter = await createCountingDO();
    process.env.NINEROUTER_D1_PROFILE = "extreme";
    const cv = await import("@/lib/coordinator/controlVersion.js");
    cv.__test__.reset();
    expect(await cv.getControlVersion("settings")).toBe(0);
    expect(await cv.getControlVersion("apiKeys")).toBe(0);
    let snap = holder.counter.snapshot();
    // first call knew one domain; the second domain was fetched on its first use
    expect(snap.byMethod.getVersions).toBe(2);
    holder.counter.reset();

    await cv.bumpControlVersion("apiKeys");
    holder.counter.reset();
    // inside the poll interval: no RPC at all
    expect(await cv.getControlVersion("apiKeys")).toBe(0);
    expect(holder.counter.snapshot().rpcs).toBe(0);
    // after the interval one RPC refreshes both domains
    vi.useFakeTimers({ toFake: ["Date"], now: Date.now() + cv.__test__.pollIntervalMs() + 1 });
    try {
      expect(await cv.getControlVersion("apiKeys")).toBe(1);
      expect(await cv.getControlVersion("settings")).toBe(0);
      snap = holder.counter.snapshot();
      expect(snap.byMethod.getVersions).toBe(1);
      expect(snap.rpcs).toBe(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("the extreme poll interval is longer than the standard one and configurable", async () => {
    const cv = await import("@/lib/coordinator/controlVersion.js");
    process.env.NINEROUTER_D1_PROFILE = "standard";
    expect(cv.__test__.pollIntervalMs()).toBe(2000);
    process.env.NINEROUTER_D1_PROFILE = "extreme";
    expect(cv.__test__.pollIntervalMs()).toBe(10_000);
    process.env.NINEROUTER_CONTROL_POLL_MS = "3000";
    expect(cv.__test__.pollIntervalMs()).toBe(3000);
    delete process.env.NINEROUTER_CONTROL_POLL_MS;
  });

  it("falls back to getVersion when the coordinator predates getVersions", async () => {
    process.env.NINEROUTER_D1_PROFILE = "extreme";
    const calls = [];
    holder.counter = {
      namespace: {
        getByName: () => ({
          getVersion: async ({ domain }) => { calls.push(["getVersion", domain]); return { version: 7 }; },
        }),
      },
    };
    const cv = await import("@/lib/coordinator/controlVersion.js");
    cv.__test__.reset();
    expect(await cv.getControlVersion("apiKeys")).toBe(7);
    expect(calls).toEqual([["getVersion", "apiKeys"]]);
  });
});

describe("write profile selection", () => {
  it("defaults to extreme on Cloudflare and standard elsewhere; env overrides both ways", async () => {
    const { writeProfile } = await import("@/lib/db/writeProfile.js");
    expect(writeProfile()).toBe("extreme");
    process.env.NINEROUTER_D1_PROFILE = "standard";
    expect(writeProfile()).toBe("standard");
    delete process.env.NINEROUTER_D1_PROFILE;
    process.env.NINEROUTER_RUNTIME = "node";
    expect(writeProfile()).toBe("standard");
    process.env.NINEROUTER_D1_PROFILE = "extreme";
    expect(writeProfile()).toBe("extreme");
    process.env.NINEROUTER_D1_PROFILE = "bogus";
    expect(writeProfile()).toBe("standard");
  });
});
