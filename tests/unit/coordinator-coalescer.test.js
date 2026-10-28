import { describe, expect, it, vi } from "vitest";
import { createCoalescer } from "@/lib/coordinator/coalescer.js";

function setup(overrides = {}) {
  let clock = 1_000;
  const call = vi.fn(async () => ({ ok: true }));
  const tunables = { coordinatorReadTtlMs: 5_000, coordinatorFlushMs: 30_000, coordinatorFlushMaxEvents: 500, ...overrides };
  const c = createCoalescer({ call, now: () => clock, tunables });
  return { c, call, tick: (ms) => { clock += ms; } };
}

describe("coalescer reads", () => {
  it("serves repeat reads from the cache until the TTL passes", async () => {
    const { c, tick } = setup();
    const loader = vi.fn(async () => ({ locks: [] }));
    await c.read("k", "provider:x", loader);
    await c.read("k", "provider:x", loader);
    expect(loader).toHaveBeenCalledTimes(1);
    tick(5_001);
    await c.read("k", "provider:x", loader);
    expect(loader).toHaveBeenCalledTimes(2);
  });

  it("shares one in-flight load between concurrent callers", async () => {
    const { c } = setup();
    let release;
    const loader = vi.fn(() => new Promise((resolve) => { release = () => resolve("v"); }));
    const a = c.read("k", "s", loader);
    const b = c.read("k", "s", loader);
    await Promise.resolve();
    release();
    expect(await Promise.all([a, b])).toEqual(["v", "v"]);
    expect(loader).toHaveBeenCalledTimes(1);
  });

  it("invalidate(scope) drops only that scope", async () => {
    const { c } = setup();
    const a = vi.fn(async () => 1);
    const b = vi.fn(async () => 2);
    await c.read("a", "scope-a", a);
    await c.read("b", "scope-b", b);
    c.invalidate("scope-a");
    await c.read("a", "scope-a", a);
    await c.read("b", "scope-b", b);
    expect(a).toHaveBeenCalledTimes(2);
    expect(b).toHaveBeenCalledTimes(1);
  });

  it("a TTL of 0 disables caching", async () => {
    const { c } = setup({ coordinatorReadTtlMs: 0 });
    const loader = vi.fn(async () => 1);
    await c.read("k", "s", loader);
    await c.read("k", "s", loader);
    expect(loader).toHaveBeenCalledTimes(2);
  });

  it("does not cache a failed load", async () => {
    const { c } = setup();
    const loader = vi.fn().mockRejectedValueOnce(new Error("down")).mockResolvedValue("ok");
    await expect(c.read("k", "s", loader)).rejects.toThrow("down");
    expect(await c.read("k", "s", loader)).toBe("ok");
  });
});

describe("coalescer writes", () => {
  it("aggregates outcomes per (provider, connection, model) and sends one call per provider plus one metrics call", async () => {
    const { c, call } = setup();
    for (let i = 0; i < 10; i++) c.queueOutcome({ provider: "openai", connectionId: "a", model: "m", success: true, latencyMs: 100, status: 200, costUsd: 0.5 });
    c.queueOutcome({ provider: "openai", connectionId: "b", model: "m", success: false, latencyMs: 50, status: 500 });
    c.queueOutcome({ provider: "claude", connectionId: "c", model: "m", success: true, latencyMs: 10, status: 200 });
    for (let i = 0; i < 12; i++) c.queueMetrics({ tenant: "t", provider: "openai", connectionId: "a", model: "m", success: true, status: 200, latencyMs: 100 });

    expect(call).not.toHaveBeenCalled();
    const result = await c.flush();
    expect(result.rpcs).toBe(3); // openai + claude + metrics
    const byMethod = Object.fromEntries(call.mock.calls.map(([method, payload, scope]) => [`${method}@${scope}`, payload]));
    const openai = byMethod["recordOutcomes@provider:openai"].items;
    expect(openai).toHaveLength(2);
    expect(openai.find((i) => i.connectionId === "a")).toMatchObject({ samples: 10, successes: 10, failures: 0, latencyCount: 10, latencySumMs: 1000, costUsd: 5 });
    expect(openai.find((i) => i.connectionId === "b")).toMatchObject({ samples: 1, failures: 1, tailFailures: 1, lastStatus: 500 });
    expect(byMethod["recordMetricsBatch@metrics"].items[0]).toMatchObject({ successes: 12, latencyCount: 12, latencyMaxMs: 100 });
  });

  it("tracks the trailing failure run", async () => {
    const { c, call } = setup();
    for (const success of [false, false, true, false, false, false]) c.queueOutcome({ provider: "p", connectionId: "a", model: "m", success });
    await c.flush();
    expect(call.mock.calls[0][1].items[0]).toMatchObject({ successes: 1, failures: 5, tailFailures: 3 });
  });

  it("flushes by itself when the event ceiling is reached", async () => {
    const { c, call } = setup({ coordinatorFlushMaxEvents: 5 });
    for (let i = 0; i < 5; i++) c.queueOutcome({ provider: "p", connectionId: "a", model: "m", success: true });
    await c.flush(); // joins the in-flight flush started by the 5th event
    expect(call).toHaveBeenCalledTimes(1);
    expect(c.pending().events).toBe(0);
  });

  it("flushes by itself once the interval has elapsed", async () => {
    const waitUntil = vi.fn();
    let clock = 0;
    const call = vi.fn(async () => ({}));
    const c = createCoalescer({ call, now: () => clock, waitUntil, tunables: { coordinatorReadTtlMs: 0, coordinatorFlushMs: 30_000, coordinatorFlushMaxEvents: 500 } });
    c.queueOutcome({ provider: "p", connectionId: "a", model: "m", success: true });
    clock = 30_001;
    c.queueOutcome({ provider: "p", connectionId: "a", model: "m", success: true });
    expect(waitUntil).toHaveBeenCalledTimes(1);
    await waitUntil.mock.calls[0][0];
    expect(call.mock.calls[0][1].items[0].samples).toBe(2);
  });

  it("a failing coordinator is reported, never thrown into the request", async () => {
    const onError = vi.fn();
    const c = createCoalescer({ call: async () => { throw new Error("down"); }, onError, tunables: { coordinatorReadTtlMs: 0, coordinatorFlushMs: 1e9, coordinatorFlushMaxEvents: 1e9 } });
    c.queueOutcome({ provider: "p", connectionId: "a", model: "m", success: true });
    await expect(c.flush()).resolves.toBeTruthy();
    expect(onError).toHaveBeenCalled();
  });

  it("an empty flush sends nothing", async () => {
    const { c, call } = setup();
    await c.flush();
    expect(call).not.toHaveBeenCalled();
  });
});
