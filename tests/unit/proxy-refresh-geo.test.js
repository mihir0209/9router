import { describe, expect, it, vi, beforeEach } from "vitest";

vi.mock("@/lib/proxy/providers/registry.js", () => ({
  getProvider: vi.fn(),
  toPoolEntries: vi.fn((proxies, source) =>
    proxies.map((p) => ({ ...p, source, id: `test-${p.proxyUrl}` }))
  ),
}));

vi.mock("@/lib/proxy/validateProxies.js", () => ({
  validateProxyList: vi.fn(),
  FETCH_TIMEOUT_MS: 30000,
}));

vi.mock("@/lib/proxy/geoTargets.js", () => ({
  getLowUsageCountryCodes: vi.fn(),
}));

vi.mock("@/lib/localDb", () => ({
  replaceProxyPoolsBySource: vi.fn(),
  getSettings: vi.fn(async () => ({})),
  updateSettings: vi.fn(),
}));

import { getProvider, toPoolEntries } from "@/lib/proxy/providers/registry.js";
import { validateProxyList } from "@/lib/proxy/validateProxies.js";
import { getLowUsageCountryCodes } from "@/lib/proxy/geoTargets.js";
import { replaceProxyPoolsBySource } from "@/lib/localDb";
import { refreshProviderPool } from "@/lib/proxy/refreshPool.js";

const provider = {
  id: "geonode",
  name: "GeoNode",
  normalizeConfig: vi.fn((input) => ({ minWorkingProxies: 1, ...input })),
  fetch: vi.fn(),
};

function mockValidation(allOk = true) {
  validateProxyList.mockImplementation(async (proxies) => {
    const working = proxies
      .filter((_, i) => allOk || i % 2 === 0)
      .map((p) => ({ ...p, validation: { status: "ok", elapsedMs: 12, ok: true } }));
    return {
      checkedCount: proxies.length,
      workingCount: working.length,
      failedCount: proxies.length - working.length,
      working,
      results: [],
    };
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  provider.normalizeConfig.mockImplementation((input) => ({ minWorkingProxies: 1, ...input }));
  getProvider.mockResolvedValue(provider);
  replaceProxyPoolsBySource.mockImplementation(async (_source, pools) => pools);
  mockValidation(true);
});

describe("refreshProviderPool — geo targeting off", () => {
  it("fetches once with the normalized config and honors validateImported=false", async () => {
    provider.fetch.mockResolvedValue([{ proxyUrl: "http://1.1.1.1:80" }]);
    const config = { enabled: true, geoTargeting: "off", validateImported: false };

    const result = await refreshProviderPool("geonode", config);

    expect(provider.fetch).toHaveBeenCalledTimes(1);
    expect(provider.fetch).toHaveBeenCalledWith(expect.objectContaining({ geoTargeting: "off" }));
    expect(validateProxyList).not.toHaveBeenCalled();
    expect(result.importedCount).toBe(1);
  });
});

describe("refreshProviderPool — OpenCode geo targeting", () => {
  it("fans out one fetch per target country and tags untagged proxies", async () => {
    getLowUsageCountryCodes.mockResolvedValue(["SB", "KM"]);
    provider.fetch.mockImplementation(async (config) =>
      config.country === "SB"
        ? [{ proxyUrl: "http://sb-1:8080" }]
        : [{ proxyUrl: "http://km-1:8080", country: "Comoros" }]
    );

    await refreshProviderPool("geonode", {
      enabled: true,
      geoTargeting: "opencode",
      geoMaxCountries: 3,
      validateImported: false,
    });

    expect(getLowUsageCountryCodes).toHaveBeenCalledWith({ limit: 3 });
    expect(provider.fetch).toHaveBeenCalledTimes(2);
    expect(provider.fetch.mock.calls[0][0].country).toBe("SB");
    expect(provider.fetch.mock.calls[1][0].country).toBe("KM");

    const imported = toPoolEntries.mock.calls[0][0];
    expect(imported.map(({ proxyUrl, country }) => ({ proxyUrl, country }))).toEqual([
      { proxyUrl: "http://sb-1:8080", country: "SB" },
      { proxyUrl: "http://km-1:8080", country: "Comoros" },
    ]);
  });

  it("forces validation even when validateImported=false (active-only classifier)", async () => {
    getLowUsageCountryCodes.mockResolvedValue(["SB"]);
    provider.fetch.mockResolvedValue([
      { proxyUrl: "http://sb-1:8080" },
      { proxyUrl: "http://sb-2:8080" },
    ]);

    await refreshProviderPool("geonode", {
      enabled: true,
      geoTargeting: "opencode",
      validateImported: false,
    });

    expect(validateProxyList).toHaveBeenCalledTimes(1);
    expect(validateProxyList.mock.calls[0][0]).toHaveLength(2);
    const imported = toPoolEntries.mock.calls[0][0];
    expect(imported.every((p) => p.validation?.ok)).toBe(true);
  });

  it("dedupes proxies returned by multiple countries", async () => {
    getLowUsageCountryCodes.mockResolvedValue(["SB", "KM"]);
    provider.fetch.mockImplementation(async (config) => [
      { proxyUrl: `http://${config.country.toLowerCase()}-only:80` },
      { proxyUrl: "http://shared-dup:80" },
    ]);

    await refreshProviderPool("geonode", { enabled: true, geoTargeting: "opencode" });

    const imported = toPoolEntries.mock.calls[0][0];
    expect(imported.filter((p) => p.proxyUrl === "http://shared-dup:80")).toHaveLength(1);
    expect(imported).toHaveLength(3);
  });

  it("isolates per-country failures and keeps the successful countries", async () => {
    getLowUsageCountryCodes.mockResolvedValue(["BAD", "GOOD"]);
    provider.fetch.mockImplementation(async (config) => {
      if (config.country === "BAD") throw new Error("upstream 500");
      return [{ proxyUrl: "http://good-1:80" }];
    });

    const result = await refreshProviderPool("geonode", { enabled: true, geoTargeting: "opencode" });

    expect(result.importedCount).toBe(1);
    const imported = toPoolEntries.mock.calls[0][0];
    expect(imported[0].proxyUrl).toBe("http://good-1:80");
  });

  it("throws when every country fails", async () => {
    getLowUsageCountryCodes.mockResolvedValue(["BAD1", "BAD2"]);
    provider.fetch.mockRejectedValue(new Error("upstream down"));

    await expect(
      refreshProviderPool("geonode", { enabled: true, geoTargeting: "opencode" })
    ).rejects.toThrow(/no proxies for any of 2 target countries/);
  });

  it("throws when no targets are available yet", async () => {
    getLowUsageCountryCodes.mockResolvedValue([]);

    await expect(
      refreshProviderPool("geonode", { enabled: true, geoTargeting: "opencode" })
    ).rejects.toThrow(/no target countries are available/);
    expect(provider.fetch).not.toHaveBeenCalled();
  });

  it("keeps stats fields in the updated provider config", async () => {
    getLowUsageCountryCodes.mockResolvedValue(["SB"]);
    provider.fetch.mockResolvedValue([{ proxyUrl: "http://sb-1:80" }]);

    const result = await refreshProviderPool("geonode", { enabled: true, geoTargeting: "opencode" });

    expect(result.config.lastRefreshAt).toBeTruthy();
    expect(result.config.lastError).toBeNull();
    expect(result.config.lastWorkingCount).toBe(1);
    expect(result.config.geoTargeting).toBe("opencode");
  });
});
