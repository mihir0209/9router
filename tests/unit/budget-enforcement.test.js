import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  enforce: vi.fn(),
  requireApiKey: true,
}));

vi.mock("@/lib/localDb", () => ({
  getSettings: async () => ({ requireApiKey: mocks.requireApiKey }),
  getApiKeyByKey: async () => ({ rateLimitRequests: 0 }),
  updateApiKey: vi.fn(),
}));
vi.mock("../../src/sse/services/auth.js", () => ({
  extractApiKey: () => "sk-budget",
  isValidApiKey: async () => true,
}));
vi.mock("../../src/sse/services/apiKeyEnforcement.js", () => ({
  enforceApiKeyRestrictions: mocks.enforce,
}));
vi.mock("../../src/sse/utils/logger.js", () => ({ warn: vi.fn(), error: vi.fn() }));

import { validateAndEnforceApiKey } from "../../src/sse/services/validateApiKey.js";

describe("budget enforcement response", () => {
  beforeEach(() => {
    mocks.requireApiKey = true;
    mocks.enforce.mockReset();
  });

  it("returns an OpenAI-style 402 with budget headers", async () => {
    mocks.enforce.mockResolvedValue({
      allowed: false,
      status: 402,
      error: "Budget exceeded for workspace key. Add credits to continue.",
      budget: { enabled: true, limit: 10, used: 10, remaining: 0, resetsAt: "2099-08-02T00:00:00.000Z" },
    });

    const result = await validateAndEnforceApiKey(new Request("http://localhost/v1/chat/completions"), {});
    const body = await result.response.json();

    expect(result.response.status).toBe(402);
    expect(body).toEqual({
      error: {
        message: "Budget exceeded for workspace key. Add credits to continue.",
        type: "budget_exceeded",
        param: null,
        code: 402,
      },
    });
    expect(result.response.headers.get("x-budget-remaining")).toBe("0");
    expect(result.response.headers.get("x-budget-reset")).toBe("2099-08-02T00:00:00.000Z");
    expect(Number(result.response.headers.get("retry-after"))).toBeGreaterThan(0);
  });

  it("enforces a recognized key when global key authentication is optional", async () => {
    mocks.requireApiKey = false;
    mocks.enforce.mockResolvedValue({ allowed: false, status: 402, error: "Budget exceeded", budget: {
      enabled: true, limit: 0, used: 0, remaining: 0, exceeded: true, resetsAt: null,
    } });

    const result = await validateAndEnforceApiKey(new Request("http://localhost/v1/chat/completions"), {});

    expect(result.response.status).toBe(402);
    expect(mocks.enforce).toHaveBeenCalledWith("sk-budget", expect.any(Request), {});
  });
});
