/**
 * Observability durability tests.
 *
 * Two guarantees are pinned here:
 *  1. Audit rows are written before the admin request returns (no fire-and-forget,
 *     which a Workers isolate can drop on eviction).
 *  2. Metrics/alerts aggregate through the Durable Object so the admin view is
 *     cluster-wide instead of one isolate's memory.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  run: vi.fn(),
  all: vi.fn(),
  get: vi.fn(),
  coordinatorCall: vi.fn(),
  insertCount: 0,
}));

vi.mock("@/lib/db/driver.js", () => ({
  getAdapter: async () => ({
    run: (...args) => { mocks.insertCount += 1; return mocks.run(...args); },
    all: (...args) => mocks.all(...args),
    get: (...args) => mocks.get(...args),
  }),
}));

vi.mock("@/lib/coordinator/client.js", () => ({ coordinatorCall: mocks.coordinatorCall }));

const { appendAuditEvent, listAuditEvents } = await import("@/lib/db/repos/auditRepo.js");
const { recordRequestOutcome, getMetricsSnapshot, getAlerts, resetMetrics, prometheusMetrics, recordMetric } = await import("@/lib/observability/metrics.js");

const auditRow = {
  id: "a1",
  timestamp: "2026-01-01T00:00:00.000Z",
  actor: "admin-token",
  action: "api_key.created",
  resourceType: "api_key",
  resourceId: "key-1",
  ownerRef: "acme",
  requestId: null,
  sourceIp: "203.0.113.5",
  outcome: "success",
  before: null,
  after: null,
  metadata: "{}",
};

beforeEach(() => {
  vi.clearAllMocks();
  mocks.insertCount = 0;
  mocks.run.mockReturnValue({ changes: 1 });
  mocks.all.mockResolvedValue([auditRow]);
  process.env.NINEROUTER_RUNTIME = "node";
  resetMetrics();
});

describe("audit log durability", () => {
  it("awaits the insert before returning", async () => {
    let resolved = false;
    mocks.run.mockImplementation(async () => { await new Promise((r) => setTimeout(r, 10)); resolved = true; return { changes: 1 }; });

    await appendAuditEvent({ actor: "admin-token", action: "api_key.created", resourceType: "api_key", resourceId: "key-1", ownerRef: "acme" });
    expect(resolved).toBe(true);
    expect(mocks.insertCount).toBe(1);
  });

  it("propagates write failures instead of silently dropping the audit row", async () => {
    mocks.run.mockRejectedValue(new Error("disk full"));
    await expect(appendAuditEvent({ action: "api_key.created" })).rejects.toThrow("disk full");
  });

  it("truncates oversized before/after payloads", async () => {
    await appendAuditEvent({ action: "api_key.updated", before: { blob: "x".repeat(30_000) } });
    const params = mocks.run.mock.calls[0][1];
    const before = JSON.parse(params[10]);
    expect(before.truncated).toBe(true);
    expect(before.preview.length).toBeLessThanOrEqual(16_000);
  });

  it("clamps caller-supplied free-text fields", async () => {
    await appendAuditEvent({ action: "x".repeat(500), actor: "y".repeat(500), sourceIp: "z".repeat(500) });
    const params = mocks.run.mock.calls[0][1];
    expect(params[2].length).toBeLessThanOrEqual(200);
    expect(params[3].length).toBeLessThanOrEqual(120);
    expect(params[9].length).toBeLessThanOrEqual(100);
  });

  it("awaits reads and maps rows", async () => {
    const events = await listAuditEvents({ ownerRef: "acme" });
    expect(events).toHaveLength(1);
    expect(events[0]).toMatchObject({ action: "api_key.created", ownerRef: "acme" });
  });
});

describe("metrics durability", () => {
  it("aggregates provider outcomes in the local process", async () => {
    recordRequestOutcome({ provider: "openai", connectionId: "a1", model: "gpt-5", success: true, status: 200, latencyMs: 100 });
    recordRequestOutcome({ provider: "openai", connectionId: "a1", model: "gpt-5", success: false, status: 500, latencyMs: 300 });

    const snapshot = await getMetricsSnapshot();
    expect(snapshot.providers).toHaveLength(1);
    expect(snapshot.providers[0]).toMatchObject({ successes: 1, failures: 1, consecutiveFailures: 1 });
    expect(snapshot.latency.count).toBe(2);
    expect(snapshot.latency.averageMs).toBe(200);
  });

  it("raises a warning then a critical alert as failures accumulate", async () => {
    for (let i = 0; i < 3; i++) recordRequestOutcome({ provider: "openai", connectionId: "a1", model: "gpt-5", success: false, status: 500 });
    let alerts = await getAlerts();
    expect(alerts).toHaveLength(1);
    expect(alerts[0]).toMatchObject({ severity: "warning", provider: "openai" });

    for (let i = 0; i < 2; i++) recordRequestOutcome({ provider: "openai", connectionId: "a1", model: "gpt-5", success: false, status: 500 });
    alerts = await getAlerts();
    expect(alerts[0].severity).toBe("critical");
  });

  it("clears the alert once the provider recovers", async () => {
    for (let i = 0; i < 3; i++) recordRequestOutcome({ provider: "openai", connectionId: "a1", model: "gpt-5", success: false, status: 500 });
    expect(await getAlerts()).toHaveLength(1);

    recordRequestOutcome({ provider: "openai", connectionId: "a1", model: "gpt-5", success: true, status: 200 });
    expect(await getAlerts()).toHaveLength(0);
  });

  it("mirrors every outcome to the Durable Object on Cloudflare", async () => {
    process.env.NINEROUTER_RUNTIME = "cloudflare";
    mocks.coordinatorCall.mockResolvedValue({ recorded: true });

    recordRequestOutcome({ provider: "openai", connectionId: "a1", model: "gpt-5", success: true, status: 200, latencyMs: 120 });
    await vi.waitFor(() => expect(mocks.coordinatorCall).toHaveBeenCalled());

    const [method, payload, scope] = mocks.coordinatorCall.mock.calls[0];
    expect(method).toBe("recordMetrics");
    expect(scope).toBe("metrics");
    expect(payload.outcome).toMatchObject({ provider: "openai", connectionId: "a1", model: "gpt-5", success: true, status: 200, latencyMs: 120 });
  });

  it("prefers the cluster-wide Durable Object view on Cloudflare", async () => {
    process.env.NINEROUTER_RUNTIME = "cloudflare";
    mocks.coordinatorCall.mockResolvedValue({
      startedAt: 1,
      counters: [{ name: "provider_requests_total", labels: {}, value: 42 }],
      providers: [{ provider: "anthropic", connectionId: "z9", model: "claude", successes: 40, failures: 2, consecutiveFailures: 0, lastStatus: 200, lastLatencyMs: 90 }],
      latency: { count: 42, sumMs: 4_000, maxMs: 900, averageMs: 95 },
      alerts: [],
    });

    const snapshot = await getMetricsSnapshot();
    expect(snapshot.counters[0].value).toBe(42);
    expect(snapshot.providers[0].provider).toBe("anthropic");
    expect(snapshot.latency.averageMs).toBe(95);
  });

  it("falls back to the local snapshot when the coordinator is unreachable", async () => {
    process.env.NINEROUTER_RUNTIME = "cloudflare";
    recordMetric("local_only_total", 2);
    mocks.coordinatorCall.mockRejectedValue(new Error("coordinator down"));

    const snapshot = await getMetricsSnapshot();
    expect(snapshot.counters.some((c) => c.name === "local_only_total")).toBe(true);
  });

  it("renders prometheus output from the merged snapshot", async () => {
    recordRequestOutcome({ provider: "openai", connectionId: "a1", model: "gpt-5", success: true, status: 200, latencyMs: 100 });
    const text = prometheusMetrics(await getMetricsSnapshot());
    // Tenant leads the label set; this outcome carries no API key, so it is
    // attributed to "anonymous" rather than dropped.
    expect(text).toContain('provider_successes_total{tenant="anonymous",provider="openai",model="gpt-5",connection="a1"} 1');
    expect(text).toContain("gateway_latency_average_ms 100");
  });
});
