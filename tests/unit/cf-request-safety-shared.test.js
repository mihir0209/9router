// Dead-owner scenarios for the remaining module-level shared work on the Cloudflare hot path:
// control-version poll, coordinator read cache / flush, token-refresh dedup.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({ coordinatorCall: vi.fn() }));
vi.mock("@/lib/coordinator/client.js", () => ({ coordinatorCall: mocks.coordinatorCall }));

const NEVER = () => new Promise(() => {});

beforeEach(() => {
  process.env.NINEROUTER_RUNTIME = "cloudflare";
  process.env.NINEROUTER_FLIGHT_WAIT_MS = "40";
  mocks.coordinatorCall.mockReset();
});
afterEach(() => {
  delete process.env.NINEROUTER_RUNTIME;
  delete process.env.NINEROUTER_FLIGHT_WAIT_MS;
});

describe("control version poll", () => {
  it("a poll owned by a cancelled request does not hang later requests", async () => {
    vi.resetModules();
    const { getControlVersion, __test__ } = await import("../../src/lib/coordinator/controlVersion.js");
    __test__.reset();
    mocks.coordinatorCall.mockImplementationOnce(NEVER);
    mocks.coordinatorCall.mockImplementation(async () => ({ versions: { apiKeys: 7 }, version: 7 }));
    getControlVersion("apiKeys"); // owner: dead
    const started = Date.now();
    const v = await getControlVersion("apiKeys");
    expect(v).toBe(7);
    expect(Date.now() - started).toBeLessThan(2000);
  });

  it("concurrent callers share one coordinator call while the owner is alive", async () => {
    vi.resetModules();
    const { getControlVersion, __test__ } = await import("../../src/lib/coordinator/controlVersion.js");
    __test__.reset();
    mocks.coordinatorCall.mockImplementation(async () => { await new Promise((r) => setTimeout(r, 10)); return { versions: { settings: 3 } }; });
    const out = await Promise.all([getControlVersion("settings"), getControlVersion("settings"), getControlVersion("settings")]);
    expect(out).toEqual([3, 3, 3]);
    expect(mocks.coordinatorCall).toHaveBeenCalledTimes(1);
  });
});

describe("coordinator coalescer", () => {
  it("a cached read whose loader never answers does not hang the next read", async () => {
    const { createCoalescer } = await import("../../src/lib/coordinator/coalescer.js");
    const c = createCoalescer({ call: async () => ({}), tunables: { coordinatorReadTtlMs: 5000, coordinatorFlushMs: 30000, coordinatorFlushMaxEvents: 500 } });
    c.read("k", "s", NEVER);
    await expect(c.read("k", "s", async () => "fresh")).resolves.toBe("fresh");
  });

  it("a flush that never settles does not block later flushes forever", async () => {
    const { createCoalescer } = await import("../../src/lib/coordinator/coalescer.js");
    let n = 0;
    const calls = [];
    const c = createCoalescer({
      call: (method) => { calls.push(method); return n++ === 0 ? NEVER() : Promise.resolve({}); },
      tunables: { coordinatorReadTtlMs: 5000, coordinatorFlushMs: 30000, coordinatorFlushMaxEvents: 500 },
    });
    c.queueOutcome({ provider: "p", connectionId: "c", model: "m", success: true, latencyMs: 5, status: 200 });
    c.flush(); // owner: dead
    c.queueOutcome({ provider: "p", connectionId: "c", model: "m", success: true, latencyMs: 5, status: 200 });
    const started = Date.now();
    const joined = await c.flush();
    expect(Date.now() - started).toBeLessThan(5000);
    expect(joined).toBeTruthy();
  });
});

describe("token refresh dedup", () => {
  it("a refresh owned by a cancelled request does not block the next refresh of the same token", async () => {
    vi.resetModules();
    const { dedupRefresh } = await import("../../open-sse/services/tokenRefresh/dedup.js");
    dedupRefresh("codex", "old-token", NEVER);
    const out = await dedupRefresh("codex", "old-token", async () => ({ accessToken: "new" }));
    expect(out).toEqual({ accessToken: "new" });
  });

  it("returns the same result for concurrent refreshes and caches the result briefly", async () => {
    vi.resetModules();
    const { dedupRefresh } = await import("../../open-sse/services/tokenRefresh/dedup.js");
    let calls = 0;
    const fn = async () => { calls += 1; await new Promise((r) => setTimeout(r, 10)); return { accessToken: "x" }; };
    const [a, b] = await Promise.all([dedupRefresh("codex", "t1", fn), dedupRefresh("codex", "t1", fn)]);
    expect(a).toEqual(b);
    await dedupRefresh("codex", "t1", fn);
    expect(calls).toBe(1);
  });
});
