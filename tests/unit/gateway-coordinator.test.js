import { beforeEach, describe, expect, it } from "vitest";
import { __test__, getAdaptiveScores, scoreProviderEntries } from "@/lib/coordinator/client.js";

beforeEach(() => {
  __test__.memoryState().rate.clear();
  __test__.memoryState().active.clear();
  __test__.memoryState().locks.clear();
  __test__.memoryState().scores.clear();
  __test__.memoryState().budgets.clear();
});

describe("gateway coordinator memory fallback", () => {
  it("enforces rate limits and concurrency leases", () => {
    expect(__test__.memoryCall("checkRateLimit", { keyId: "key-1", limit: 2, windowMs: 60_000, now: 1_000 })).toMatchObject({ allowed: true, count: 1, remaining: 1 });
    expect(__test__.memoryCall("checkRateLimit", { keyId: "key-1", limit: 2, windowMs: 60_000, now: 1_001 })).toMatchObject({ allowed: true, count: 2, remaining: 0 });
    expect(__test__.memoryCall("checkRateLimit", { keyId: "key-1", limit: 2, windowMs: 60_000, now: 1_002 })).toMatchObject({ allowed: false, count: 3, remaining: 0 });

    const first = __test__.memoryCall("beginRequest", { keyId: "key-1", maxConcurrent: 1, now: 2_000 });
    expect(first.allowed).toBe(true);
    expect(__test__.memoryCall("beginRequest", { keyId: "key-1", maxConcurrent: 1, now: 2_001 }).allowed).toBe(false);
    expect(__test__.memoryCall("endRequest", { keyId: "key-1", leaseId: first.leaseId })).toEqual({ active: 0 });
  });

  it("shares provider locks and clears expired locks", () => {
    __test__.memoryCall("setLock", { provider: "openai", connectionId: "account-1", model: "gpt-5", untilMs: 4_000, status: 429, reason: "quota" });
    expect(__test__.memoryCall("getActiveLocks", { provider: "openai", connectionIds: ["account-1"], model: "gpt-5", now: 3_000 })).toHaveLength(1);
    expect(__test__.memoryCall("getActiveLocks", { provider: "openai", connectionIds: ["account-1"], model: "gpt-5", now: 5_000 })).toHaveLength(0);
    __test__.memoryCall("setLock", { provider: "openai", connectionId: "account-1", model: "gpt-5", untilMs: 6_000 });
    __test__.memoryCall("clearLock", { provider: "openai", connectionId: "account-1", model: "gpt-5" });
    expect(__test__.memoryCall("getActiveLocks", { provider: "openai", connectionIds: ["account-1"], model: "gpt-5", now: 5_500 })).toHaveLength(0);
  });

  it("records outcomes and ranks observed providers adaptively", async () => {
    __test__.memoryCall("recordOutcome", { provider: "openai", connectionId: "account-1", model: "gpt-5", success: true, latencyMs: 100, status: 200, now: 1_000 });
    __test__.memoryCall("recordOutcome", { provider: "openai", connectionId: "account-1", model: "gpt-5", success: true, latencyMs: 100, status: 200, now: 1_100 });
    __test__.memoryCall("recordOutcome", { provider: "openai", connectionId: "account-2", model: "gpt-5", success: false, latencyMs: 2_000, status: 500, now: 1_200 });

    const scores = await getAdaptiveScores("openai", "gpt-5", ["account-1", "account-2", "account-3"]);
    const priorities = new Map([["account-1", 2], ["account-2", 1], ["account-3", 1]]);
    const ranked = scoreProviderEntries([...scores.values()], priorities);
    expect(ranked.map((entry) => entry.connectionId)).toEqual(["account-1", "account-3", "account-2"]);
  });

  it("prevents concurrent budget reservations from exceeding the available budget", () => {
    expect(__test__.memoryCall("reserveBudget", { keyId: "key-1", requestId: "request-1", amount: 6, availableBudget: 10, expiresAt: 5_000, now: 1_000 })).toMatchObject({ reserved: true });
    expect(__test__.memoryCall("reserveBudget", { keyId: "key-1", requestId: "request-2", amount: 5, availableBudget: 10, expiresAt: 5_000, now: 1_001 })).toMatchObject({ reserved: false });
    expect(__test__.memoryCall("reserveBudget", { keyId: "key-1", requestId: "request-1", amount: 6, availableBudget: 10, expiresAt: 5_000, now: 1_002 })).toMatchObject({ reserved: true, amount: 6 });
    __test__.memoryCall("releaseBudget", { keyId: "key-1", requestId: "request-1", now: 1_003 });
    expect(__test__.memoryCall("getBudgetReservations", { keyId: "key-1", now: 1_004 })).toHaveLength(0);
  });
});
