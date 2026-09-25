/**
 * Concurrency-lease lifecycle through the request path.
 *
 * `maxConcurrentRequests` occupies a Durable Object slot that must be returned
 * exactly once — on normal completion, on client abort, and when the key is
 * disabled mid-stream. A leaked lease blocks the tenant for the full 120s TTL.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  getApiKeyById: vi.fn(),
  checkBudget: vi.fn(),
}));

vi.mock("@/lib/db/index.js", () => ({ getApiKeyById: mocks.getApiKeyById }));
vi.mock("@/lib/budgetService.js", () => ({ checkBudget: mocks.checkBudget }));

const { guardApiKeyResponse } = await import("@/sse/services/apiKeyStreamGuard.js");

const keyRecord = { id: "key-1", key: "sk-live", isActive: true };

function sseResponse(chunks) {
  const encoder = new TextEncoder();
  const stream = new ReadableStream({
    start(controller) {
      for (const chunk of chunks) controller.enqueue(encoder.encode(chunk));
      controller.close();
    },
  });
  return new Response(stream, { headers: { "content-type": "text/event-stream" } });
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.getApiKeyById.mockResolvedValue(keyRecord);
  mocks.checkBudget.mockResolvedValue({ exceeded: false, enabled: false });
});

describe("apiKeyStreamGuard — lease release", () => {
  it("releases the lease when a streamed response completes", async () => {
    const release = vi.fn();
    const response = guardApiKeyResponse(sseResponse(["data: a\n\n", "data: b\n\n"]), keyRecord, release);

    const text = await response.text();
    expect(text).toContain("data: a");
    expect(text).toContain("data: b");
    expect(release).toHaveBeenCalledTimes(1);
  });

  it("releases the lease only once even with many chunks", async () => {
    const release = vi.fn();
    const response = guardApiKeyResponse(sseResponse(Array.from({ length: 25 }, (_, i) => `data: ${i}\n\n`)), keyRecord, release);

    await response.text();
    expect(release).toHaveBeenCalledTimes(1);
  });

  it("releases the lease when the client aborts mid-stream", async () => {
    const release = vi.fn();
    const encoder = new TextEncoder();
    const stream = new ReadableStream({
      start(controller) {
        controller.enqueue(encoder.encode("data: first\n\n"));
      },
    });
    const response = guardApiKeyResponse(new Response(stream, { headers: { "content-type": "text/event-stream" } }), keyRecord, release);
    const reader = response.body.getReader();

    await reader.read();
    await reader.cancel();

    expect(release).toHaveBeenCalledTimes(1);
  });

  it("releases the lease immediately for a bodyless response", async () => {
    const release = vi.fn();
    const response = guardApiKeyResponse(new Response(null, { status: 204 }), keyRecord, release);

    expect(response.status).toBe(204);
    await vi.waitFor(() => expect(release).toHaveBeenCalledTimes(1));
  });

  it("releases the lease when there is no key record to guard", async () => {
    const release = vi.fn();
    await guardApiKeyResponse(sseResponse(["data: a\n\n"]), null, release).text();
    expect(release).toHaveBeenCalledTimes(1);
  });

  it("terminates the stream and releases when the key is disabled mid-stream", async () => {
    const release = vi.fn();
    mocks.getApiKeyById.mockResolvedValue({ ...keyRecord, isActive: false });
    const response = guardApiKeyResponse(sseResponse(["data: a\n\n", "data: b\n\n"]), keyRecord, release);

    const text = await response.text();
    expect(text).toBe("");
    expect(release).toHaveBeenCalledTimes(1);
  });

  it("stops the stream with a budget error frame when the budget is exhausted", async () => {
    const release = vi.fn();
    mocks.checkBudget.mockResolvedValue({ exceeded: true, enabled: true, limit: 1, used: 1, remaining: 0, resetsAt: null });
    const response = guardApiKeyResponse(sseResponse(["data: a\n\n", "data: b\n\n"]), keyRecord, release);

    const text = await response.text();
    expect(text).toContain("budget_exceeded");
    expect(text).not.toContain("data: a");
    expect(release).toHaveBeenCalledTimes(1);
  });

  it("checks the budget at most once per second on a long stream", async () => {
    const release = vi.fn();
    const chunks = Array.from({ length: 200 }, (_, i) => `data: ${i}\n\n`);
    const response = guardApiKeyResponse(sseResponse(chunks), keyRecord, release);

    await response.text();
    expect(mocks.checkBudget.mock.calls.length).toBeLessThanOrEqual(2);
  });
});

describe("concurrency lease heartbeat", () => {
  const keyRecord = {
    id: "key-1",
    key: "sk-live",
    isActive: true,
    isActiveField: true,
    maxConcurrentRequests: 1,
  };

  let calls;

  beforeEach(() => {
    vi.useFakeTimers();
    calls = [];
    vi.resetModules();
    vi.doMock("@/lib/localDb", () => ({
      getApiKeyByKey: async () => keyRecord,
      getCombos: async () => [],
    }));
    vi.doMock("@/lib/db/repos/usageRepo.js", () => ({ getApiKeyTokenUsage: async () => 0 }));
    vi.doMock("@/lib/coordinator/client.js", () => ({
      coordinatorCall: vi.fn(async (method, payload) => {
        calls.push({ method, payload });
        if (method === "beginRequest") return { allowed: true, active: 1, leaseId: "lease-1" };
        if (method === "renewRequest") return { renewed: true, active: 1 };
        if (method === "endRequest") return { active: 0 };
        return {};
      }),
    }));
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.doUnmock("@/lib/coordinator/client.js");
  });

  async function enforce() {
    const { enforceApiKeyRestrictions } = await import("@/sse/services/apiKeyEnforcement.js");
    return await enforceApiKeyRestrictionsWithIp();
    async function enforceApiKeyRestrictionsWithIp() {
      return enforceApiKeyRestrictions("sk-live", new Request("https://x/v1/chat/completions"), { model: "openai/gpt-4o" });
    }
  }

  it("renews the lease while the request is in flight", async () => {
    const result = await enforce();
    expect(result.allowed).toBe(true);
    expect(calls.filter((c) => c.method === "beginRequest")).toHaveLength(1);
    expect(calls.filter((c) => c.method === "renewRequest")).toHaveLength(0);

    await vi.advanceTimersByTimeAsync(120_000);
    expect(calls.filter((c) => c.method === "renewRequest").length).toBeGreaterThanOrEqual(2);

    await result.release();
    const renewalsBefore = calls.filter((c) => c.method === "renewRequest").length;
    await vi.advanceTimersByTimeAsync(120_000);
    expect(calls.filter((c) => c.method === "renewRequest")).toHaveLength(renewalsBefore);
    expect(calls.filter((c) => c.method === "endRequest")).toHaveLength(1);
  });

  it("releasing twice ends the lease once", async () => {
    const result = await enforce();
    await result.release();
    await result.release();
    expect(calls.filter((c) => c.method === "endRequest")).toHaveLength(1);
  });

  it("returns a callable release, not a lease id, on a token-limit denial", async () => {
    keyRecord.tokenLimitDaily = 1;
    vi.doMock("@/lib/db/repos/usageRepo.js", () => ({ getApiKeyTokenUsage: async () => 999_999 }));
    try {
      const result = await enforce();

      expect(result.allowed).toBe(false);
      expect(result.status).toBe(429);
      expect(result.release).toBeUndefined();
      // The denial must free the slot it just took.
      expect(calls.filter((c) => c.method === "endRequest")).toHaveLength(1);
    } finally {
      keyRecord.tokenLimitDaily = 0;
    }
  });
});
