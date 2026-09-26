import { describe, it, expect, beforeEach, vi } from "vitest";

// Per-tenant observability: a degraded upstream connection is a per-tenant fact.
// The same connection can be healthy for one tenant and failing for another, and
// a cluster-wide alert cannot say who is being hurt. These cover the tenant
// dimension on the Durable Object (the only place that sees every isolate) and
// the isolation guarantee on a tenant-scoped read.

const metricsMocks = vi.hoisted(() => ({ coordinatorCall: vi.fn() }));
vi.mock("@/lib/coordinator/client.js", () => ({ coordinatorCall: metricsMocks.coordinatorCall }));

// Exercise the real Durable Object rather than a copy of its logic: the tenant
// keys and the read filter live in the DO, so testing a reimplementation would
// pass even if the DO were wrong. cloudflare:workers is stubbed because the
// class extends DurableObject, which only exists on Workers.
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
    async list({ prefix } = {}) {
      const out = new Map();
      for (const [k, v] of data) {
        if (!prefix || k.startsWith(prefix)) out.set(k, structuredClone(v));
      }
      return out;
    },
  };
}

function createDO() {
  const storage = createStorage();
  return { storage, DO: new GatewayCoordinator({ storage }, {}) };
}

const outcome = (over = {}) => ({
  provider: "openai", connectionId: "conn-1", model: "gpt-4o",
  success: true, status: 200, latencyMs: 100, ...over,
});

describe("gateway coordinator — per-tenant metrics", () => {
  let storage, DO;
  beforeEach(() => { ({ storage, DO } = createDO()); });

  it("keeps the same connection's outcomes separate per tenant", async () => {
    await DO.recordMetrics({ outcome: outcome({ tenant: "acme", success: false, status: 500 }) });
    await DO.recordMetrics({ outcome: outcome({ tenant: "acme", success: false, status: 500 }) });
    await DO.recordMetrics({ outcome: outcome({ tenant: "acme", success: false, status: 500 }) });
    await DO.recordMetrics({ outcome: outcome({ tenant: "globex", success: true }) });

    const acme = await DO.getMetrics({ tenant: "acme" });
    const globex = await DO.getMetrics({ tenant: "globex" });

    expect(acme.providers).toHaveLength(1);
    expect(acme.providers[0]).toMatchObject({ tenant: "acme", failures: 3, consecutiveFailures: 3 });
    // globex shares the upstream connection but is perfectly healthy, and must
    // not inherit acme's failure streak.
    expect(globex.providers[0]).toMatchObject({ tenant: "globex", failures: 0, consecutiveFailures: 0, successes: 1 });
  });

  it("a tenant-scoped read never returns another tenant's rows", async () => {
    await DO.recordMetrics({ outcome: outcome({ tenant: "acme", model: "gpt-4o" }) });
    await DO.recordMetrics({ outcome: outcome({ tenant: "globex", model: "claude-opus-5" }) });

    const acme = await DO.getMetrics({ tenant: "acme" });
    expect(acme.providers.map((p) => p.model)).toEqual(["gpt-4o"]);
    expect(acme.alerts.every((a) => a.tenant === "acme")).toBe(true);
  });

  it("reports per-tenant counters rather than the cluster total", async () => {
    await DO.recordMetrics({ outcome: outcome({ tenant: "acme" }) });
    await DO.recordMetrics({ outcome: outcome({ tenant: "acme" }) });
    await DO.recordMetrics({ outcome: outcome({ tenant: "globex" }) });

    const acme = await DO.getMetrics({ tenant: "acme" });
    const requests = acme.counters.find((c) => c.name === "provider_requests_total");
    expect(requests.value).toBe(2);
    expect(requests.labels.tenant).toBe("acme");

    const all = await DO.getMetrics({});
    expect(all.counters.find((c) => c.name === "provider_requests_total").value).toBe(3);
  });

  it("attributes unauthenticated traffic to anonymous rather than dropping it", async () => {
    await DO.recordMetrics({ outcome: outcome({ tenant: "" }) });
    const anon = await DO.getMetrics({ tenant: "anonymous" });
    expect(anon.providers).toHaveLength(1);
    expect(anon.providers[0].tenant).toBe("anonymous");
  });

  it("does not present cluster latency as a tenant's latency", async () => {
    await DO.recordMetrics({ outcome: outcome({ tenant: "acme", latencyMs: 250 }) });
    const acme = await DO.getMetrics({ tenant: "acme" });
    expect(acme.latency.averageMs).toBeNull();
    const all = await DO.getMetrics({});
    expect(all.latency.count).toBe(1);
  });
});

describe("metrics module — tenant labels and degradation", () => {
  let metrics;
  beforeEach(async () => {
    vi.resetModules();
    metricsMocks.coordinatorCall.mockReset();
    metrics = await import("@/lib/observability/metrics.js");
    metrics.resetMetrics();
  });

  it("labels counters and provider rows with the tenant", () => {
    metrics.recordRequestOutcome({ provider: "openai", connectionId: "c1", model: "gpt-4o", success: true, status: 200, latencyMs: 10, tenant: "acme" });
    const snap = metrics.snapshotMetrics();
    expect(snap.counters[0].labels.tenant).toBe("acme");
    expect(snap.providers[0].tenant).toBe("acme");
  });

  it("defaults missing tenant to anonymous", () => {
    metrics.recordRequestOutcome({ provider: "openai", connectionId: "c1", model: "gpt-4o", success: true, status: 200 });
    expect(metrics.snapshotMetrics().providers[0].tenant).toBe("anonymous");
  });

  it("scopes a degraded alert to the affected tenant only", async () => {
    for (let i = 0; i < 3; i++) {
      metrics.recordRequestOutcome({ provider: "openai", connectionId: "c1", model: "gpt-4o", success: false, status: 500, tenant: "acme" });
    }
    metrics.recordRequestOutcome({ provider: "openai", connectionId: "c1", model: "gpt-4o", success: true, status: 200, tenant: "globex" });

    const all = metrics.snapshotMetrics();
    expect(all.alerts).toHaveLength(1);
    expect(all.alerts[0].tenant).toBe("acme");

    const globex = await metrics.getMetricsSnapshot("globex");
    expect(globex.alerts).toHaveLength(0);
  });

  it("says degraded instead of reporting the whole cluster as one tenant", async () => {
    process.env.NINEROUTER_RUNTIME = "cloudflare";
    metricsMocks.coordinatorCall.mockRejectedValue(new Error("DO unreachable"));
    try {
      const snap = await metrics.getMetricsSnapshot("acme");
      expect(snap.degraded).toBe("durable-object-unreachable");
      expect(snap.tenant).toBe("acme");
    } finally {
      delete process.env.NINEROUTER_RUNTIME;
    }
  });

  it("emits a tenant label in the prometheus output", () => {
    metrics.recordRequestOutcome({ provider: "openai", connectionId: "c1", model: "gpt-4o", success: true, status: 200, tenant: "acme" });
    const text = metrics.prometheusMetrics();
    expect(text).toContain('tenant="acme"');
  });
});
