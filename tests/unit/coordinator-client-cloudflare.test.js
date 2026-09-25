/**
 * Cloudflare-runtime coordinator client tests.
 *
 * On Workers the client must resolve a Durable Object stub per scope and talk
 * RPC to it. These tests pin that path: enforcement calls must fail CLOSED when
 * the coordinator is unreachable (otherwise rate limits / concurrency /
 * budgets are silently bypassed on a per-isolate basis), while adaptive-scoring
 * reads degrade gracefully.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const rpcCalls = [];
const stubs = new Map();

const hoisted = vi.hoisted(() => ({ namespace: { current: null } }));

vi.mock("@opennextjs/cloudflare", () => ({
  getCloudflareContext: async () => ({ env: { GATEWAY_COORDINATOR: hoisted.namespace.current } }),
}));

const { coordinatorCall, recordProviderOutcome, getAdaptiveScores } = await import("@/lib/coordinator/client.js");
const { __test__ } = await import("@/lib/coordinator/client.js");

function makeStub(impl = {}) {
  const stub = {
    checkRateLimit: vi.fn(async (payload) => impl.checkRateLimit?.(payload) ?? { allowed: true, count: 1, remaining: 0, resetAt: 0 }),
    beginRequest: vi.fn(async (payload) => impl.beginRequest?.(payload) ?? { allowed: true, active: 1, leaseId: "lease-1" }),
    endRequest: vi.fn(async (payload) => impl.endRequest?.(payload) ?? { active: 0 }),
    getBudgetReservations: vi.fn(async (payload) => impl.getBudgetReservations?.(payload) ?? []),
    reserveBudget: vi.fn(async (payload) => impl.reserveBudget?.(payload) ?? { reserved: true, amount: payload.amount }),
    recordOutcome: vi.fn(async (payload) => impl.recordOutcome?.(payload) ?? { samples: 1 }),
    getScores: vi.fn(async (payload) => impl.getScores?.(payload) ?? []),
  };
  for (const method of Object.keys(stub)) {
    const original = stub[method];
    stub[method] = vi.fn(async (payload) => {
      rpcCalls.push({ scope: stubs.currentScope, method, payload });
      return original(payload);
    });
  }
  return stub;
}

beforeEach(() => {
  rpcCalls.length = 0;
  stubs.clear();
  process.env.NINEROUTER_RUNTIME = "cloudflare";
  __test__.memoryState().rate.clear();
  __test__.memoryState().active.clear();
  __test__.memoryState().locks.clear();
  __test__.memoryState().scores.clear();
  __test__.memoryState().budgets.clear();
  hoisted.namespace.current = {
    getByName: vi.fn((scope) => {
      stubs.currentScope = scope;
      if (!stubs.has(scope)) stubs.set(scope, makeStub());
      return stubs.get(scope);
    }),
  };
});

afterEach(() => {
  delete process.env.NINEROUTER_RUNTIME;
  vi.clearAllMocks();
});

describe("coordinator client on Cloudflare", () => {
  it("routes enforcement calls to the Durable Object for the tenant scope", async () => {
    const result = await coordinatorCall("checkRateLimit", { keyId: "k1", limit: 5, windowMs: 60_000, now: 1_000 }, "tenant:acme", { failClosed: true });

    expect(result.allowed).toBe(true);
    expect(hoisted.namespace.current.getByName).toHaveBeenCalledWith("tenant:acme");
    expect(rpcCalls).toEqual([{ scope: "tenant:acme", method: "checkRateLimit", payload: { keyId: "k1", limit: 5, windowMs: 60_000, now: 1_000 } }]);
  });

  it("keeps separate Durable Object state per tenant scope", async () => {
    await coordinatorCall("checkRateLimit", { keyId: "k1", limit: 5, windowMs: 60_000, now: 1_000 }, "tenant:acme", { failClosed: true });
    await coordinatorCall("checkRateLimit", { keyId: "k1", limit: 5, windowMs: 60_000, now: 1_000 }, "tenant:other", { failClosed: true });

    const scopes = rpcCalls.map((call) => call.scope);
    expect(scopes).toEqual(["tenant:acme", "tenant:other"]);
    expect(stubs.get("tenant:acme")).not.toBe(stubs.get("tenant:other"));
  });

  it("uses the provider scope for adaptive routing reads and writes", async () => {
    await recordProviderOutcome({ provider: "openai", connectionId: "acct-1", model: "gpt-5", success: true, status: 200, latencyMs: 120 });
    await getAdaptiveScores("openai", "gpt-5", ["acct-1"]);

    expect(rpcCalls.map((call) => call.scope)).toEqual(["provider:openai", "provider:openai"]);
  });

  it("fails closed on rate limits when the Durable Object is unreachable", async () => {
    hoisted.namespace.current = null;
    const result = await coordinatorCall("checkRateLimit", { keyId: "k1", limit: 5, windowMs: 60_000, now: 1_000 }, "tenant:acme", { failClosed: true });

    expect(result.allowed).toBe(false);
    expect(result.unavailable).toBe(true);
  });

  it("fails closed on concurrency limits when the Durable Object is unreachable", async () => {
    hoisted.namespace.current = null;
    const result = await coordinatorCall("beginRequest", { keyId: "k1", maxConcurrent: 2 }, "tenant:acme", { failClosed: true });

    expect(result.allowed).toBe(false);
    expect(result.unavailable).toBe(true);
  });

  it("fails closed on budget reservations when the Durable Object is unreachable", async () => {
    hoisted.namespace.current = null;
    const result = await coordinatorCall("reserveBudget", { keyId: "k1", requestId: "r1", amount: 5 }, "tenant:acme", { failClosed: true });

    expect(result.reserved).toBe(false);
    expect(result.unavailable).toBe(true);
  });

  it("propagates a Durable Object error instead of silently allowing the request", async () => {
    hoisted.namespace.current = {
      getByName: () => ({
        checkRateLimit: async () => { throw new Error("storage unavailable"); },
      }),
    };

    await expect(
      coordinatorCall("checkRateLimit", { keyId: "k1", limit: 5, windowMs: 60_000, now: 1_000 }, "tenant:acme", { failClosed: true }),
    ).rejects.toThrow("storage unavailable");
  });

  it("degrades to local memory for adaptive scoring when the coordinator is unreachable", async () => {
    hoisted.namespace.current = null;
    await recordProviderOutcome({ provider: "openai", connectionId: "acct-1", model: "gpt-5", success: true, status: 200, latencyMs: 120 });

    const scores = await getAdaptiveScores("openai", "gpt-5", ["acct-1"]);
    expect(scores.get("acct-1")).toMatchObject({ connectionId: "acct-1", successes: 1 });
  });

  it("uses the in-memory path outside Cloudflare even when failClosed is requested", async () => {
    process.env.NINEROUTER_RUNTIME = "node";
    const result = await coordinatorCall("checkRateLimit", { keyId: "k1", limit: 1, windowMs: 60_000, now: 1_000 }, "tenant:acme", { failClosed: true });

    expect(result.allowed).toBe(true);
    expect(hoisted.namespace.current.getByName).not.toHaveBeenCalled();
  });
});
