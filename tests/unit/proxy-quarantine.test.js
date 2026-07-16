import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@/lib/localDb", () => ({
  getSettings: vi.fn(),
  updateSettings: vi.fn(),
}));

const localDb = await import("@/lib/localDb");
const {
  buildProxyScopeKey,
  classifyProxyFailure,
  isProxyQuarantined,
  quarantineProxyPool,
} = await import("@/lib/proxy/quarantine.js");

describe("proxy quarantine", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    localDb.getSettings.mockResolvedValue({
      proxyPoolQuarantine: { global: {}, scoped: {} },
      proxyPoolRateLimitCooldownMinutes: 30,
      proxyPoolNetworkCooldownMinutes: 10,
    });
    localDb.updateSettings.mockImplementation(async (updates) => updates);
  });

  it("classifies provider rate limits as scoped and proxy/network failures as global", () => {
    expect(classifyProxyFailure(429, "FreeUsageLimitError: Rate limit exceeded")).toBe("scoped");
    expect(classifyProxyFailure(500, "fetch failed: certificate has expired")).toBe("global");
    expect(classifyProxyFailure(500, "ordinary provider error")).toBe(null);
  });

  it("matches scoped quarantine only for the same provider and model", async () => {
    await quarantineProxyPool("pool-a", {
      kind: "scoped",
      provider: "anthropic",
      model: "claude-sonnet",
      reason: "FreeUsageLimitError",
    });

    const store = localDb.updateSettings.mock.calls[0][0].proxyPoolQuarantine;

    expect(store.scoped[buildProxyScopeKey("anthropic", "claude-sonnet")]["pool-a"]).toBeTruthy();
    expect(isProxyQuarantined(store, "pool-a", { provider: "anthropic", model: "claude-sonnet" })).toBe(true);
    expect(isProxyQuarantined(store, "pool-a", { provider: "anthropic", model: "claude-haiku" })).toBe(false);
    expect(isProxyQuarantined(store, "pool-a", { provider: "openai", model: "gpt-4o" })).toBe(false);
  });

  it("matches global quarantine for any provider and model", async () => {
    await quarantineProxyPool("pool-a", {
      kind: "global",
      provider: "anthropic",
      model: "claude-sonnet",
      reason: "Connect Timeout Error",
    });

    const store = localDb.updateSettings.mock.calls[0][0].proxyPoolQuarantine;

    expect(isProxyQuarantined(store, "pool-a", { provider: "anthropic", model: "claude-sonnet" })).toBe(true);
    expect(isProxyQuarantined(store, "pool-a", { provider: "openai", model: "gpt-4o" })).toBe(true);
  });
});
