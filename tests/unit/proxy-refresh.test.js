import { describe, expect, it, vi, beforeEach } from "vitest";

// Mock the registry module
vi.mock("@/lib/proxy/providers/registry.js", () => ({
  getProvider: vi.fn(),
  toPoolEntries: vi.fn((proxies, source) =>
    proxies.map((p) => ({ ...p, source, id: `test-${p.proxyUrl}` }))
  ),
}));

// Mock validateProxies
vi.mock("@/lib/proxy/validateProxies.js", () => ({
  validateProxyList: vi.fn(),
  FETCH_TIMEOUT_MS: 30000,
}));

// Mock localDb
vi.mock("@/lib/localDb", () => ({
  replaceProxyPoolsBySource: vi.fn(),
  getSettings: vi.fn(),
  updateSettings: vi.fn(),
}));

import { getProvider, toPoolEntries } from "@/lib/proxy/providers/registry.js";
import { validateProxyList } from "@/lib/proxy/validateProxies.js";
import { replaceProxyPoolsBySource, getSettings, updateSettings } from "@/lib/localDb";
import { refreshProviderPool, getProviderConfigs, updateProviderConfig } from "@/lib/proxy/refreshPool.js";

describe("refreshProviderPool", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("awaits getProvider and calls normalizeConfig + fetch", async () => {
    const mockProvider = {
      id: "test",
      name: "Test",
      normalizeConfig: vi.fn((c) => ({ ...c, enabled: true })),
      fetch: vi.fn().mockResolvedValue([
        { proxyUrl: "http://1.2.3.4:8080", hostLabel: "1.2.3.4:8080" },
      ]),
    };

    getProvider.mockResolvedValue(mockProvider);
    replaceProxyPoolsBySource.mockResolvedValue([{ id: "test-1" }]);
    getSettings.mockResolvedValue({ proxyPoolProviders: {} });
    updateSettings.mockResolvedValue({});

    const result = await refreshProviderPool("test", { enabled: true });

    expect(getProvider).toHaveBeenCalledWith("test");
    expect(mockProvider.normalizeConfig).toHaveBeenCalledWith({ enabled: true });
    expect(mockProvider.fetch).toHaveBeenCalled();
    expect(replaceProxyPoolsBySource).toHaveBeenCalledWith("test", expect.any(Array));
    expect(result.importedCount).toBe(1);
  });

  it("throws when provider not found", async () => {
    getProvider.mockResolvedValue(null);

    await expect(refreshProviderPool("nonexistent", {})).rejects.toThrow("Unknown provider: nonexistent");
  });

  it("throws when provider returns no proxies", async () => {
    const mockProvider = {
      id: "empty",
      name: "Empty",
      normalizeConfig: vi.fn((c) => c),
      fetch: vi.fn().mockResolvedValue([]),
    };

    getProvider.mockResolvedValue(mockProvider);

    await expect(refreshProviderPool("empty", {})).rejects.toThrow("Empty returned no parseable proxies");
  });

  it("runs validation when validateImported is true", async () => {
    const mockProvider = {
      id: "test",
      name: "Test",
      normalizeConfig: vi.fn((c) => ({ ...c, validateImported: true, validationConcurrency: 5, timeoutMs: 10000, minWorkingProxies: 0 })),
      fetch: vi.fn().mockResolvedValue([
        { proxyUrl: "http://1.2.3.4:8080", hostLabel: "1.2.3.4:8080" },
      ]),
    };

    getProvider.mockResolvedValue(mockProvider);
    validateProxyList.mockResolvedValue({
      checkedCount: 1,
      workingCount: 1,
      failedCount: 0,
      working: [{ proxyUrl: "http://1.2.3.4:8080", validation: { ok: true } }],
      results: [{ proxyUrl: "http://1.2.3.4:8080", validation: { ok: true } }],
    });
    replaceProxyPoolsBySource.mockResolvedValue([{ id: "test-1" }]);
    getSettings.mockResolvedValue({ proxyPoolProviders: {} });
    updateSettings.mockResolvedValue({});

    const result = await refreshProviderPool("test", { validateImported: true });

    expect(validateProxyList).toHaveBeenCalled();
    expect(result.validationSummary.workingCount).toBe(1);
  });

  it("throws when working proxies below minimum", async () => {
    const mockProvider = {
      id: "test",
      name: "Test",
      normalizeConfig: vi.fn((c) => ({ ...c, validateImported: true, validationConcurrency: 5, timeoutMs: 10000, minWorkingProxies: 5 })),
      fetch: vi.fn().mockResolvedValue([
        { proxyUrl: "http://1.2.3.4:8080", hostLabel: "1.2.3.4:8080" },
      ]),
    };

    getProvider.mockResolvedValue(mockProvider);
    validateProxyList.mockResolvedValue({
      checkedCount: 1,
      workingCount: 1,
      failedCount: 0,
      working: [{ proxyUrl: "http://1.2.3.4:8080", validation: { ok: true } }],
      results: [{ proxyUrl: "http://1.2.3.4:8080", validation: { ok: true } }],
    });

    await expect(refreshProviderPool("test", { validateImported: true, minWorkingProxies: 5 }))
      .rejects.toThrow(/minimum required is 5/);
  });
});

describe("getProviderConfigs", () => {
  it("returns proxyPoolProviders from settings", async () => {
    getSettings.mockResolvedValue({ proxyPoolProviders: { proxyscrape: { enabled: true } } });
    const configs = await getProviderConfigs();
    expect(configs).toEqual({ proxyscrape: { enabled: true } });
  });

  it("returns empty object when no providers configured", async () => {
    getSettings.mockResolvedValue({});
    const configs = await getProviderConfigs();
    expect(configs).toEqual({});
  });
});

describe("updateProviderConfig", () => {
  it("merges config into existing proxyPoolProviders", async () => {
    getSettings.mockResolvedValue({ proxyPoolProviders: { proxyscrape: { enabled: false } } });
    updateSettings.mockResolvedValue({});

    await updateProviderConfig("proxyscrape", { enabled: true, protocol: "socks5" });

    expect(updateSettings).toHaveBeenCalledWith({
      proxyPoolProviders: {
        proxyscrape: { enabled: true, protocol: "socks5" },
      },
    });
  });
});
