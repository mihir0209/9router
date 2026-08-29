import { describe, expect, it, vi, beforeEach } from "vitest";

vi.mock("@/lib/proxy/validateProxies.js", () => ({
  validateProxyList: vi.fn(),
  FETCH_TIMEOUT_MS: 30000,
}));

vi.mock("@/lib/localDb", () => ({
  replaceProxyPoolsBySource: vi.fn(),
  getSettings: vi.fn(async () => ({})),
  updateSettings: vi.fn(),
}));

import { getProvider, getAllProviders } from "@/lib/proxy/providers/registry.js";
import { validateProxyList } from "@/lib/proxy/validateProxies.js";
import { replaceProxyPoolsBySource } from "@/lib/localDb";
import { refreshProviderPool } from "@/lib/proxy/refreshPool.js";

const KEY = "pgw-test-key";

async function loadProvider() {
  const provider = await getProvider("proxyrise");
  if (!provider) throw new Error("proxyrise provider not registered");
  return provider;
}

beforeEach(() => {
  vi.clearAllMocks();
  validateProxyList.mockImplementation(async (proxies) => ({
    checkedCount: proxies.length,
    workingCount: proxies.length,
    failedCount: 0,
    working: proxies.map((p) => ({ ...p, validation: { status: "ok", elapsedMs: 10, ok: true } })),
    results: [],
  }));
  replaceProxyPoolsBySource.mockImplementation(async (_source, pools) => pools);
});

describe("ProxyRise - normalizeConfig", () => {
  it("applies defaults", async () => {
    const provider = await loadProvider();
    const config = provider.normalizeConfig({ apiKey: KEY });
    expect(config).toMatchObject({
      apiKey: KEY,
      endpoint: "gw.proxyrise.com:443",
      type: "res",
      country: "any",
      protocol: "http",
      perCountry: 3,
      geoTargeting: "off",
      intervalMinutes: 360,
    });
  });

  it("always forces validation (test-first policy)", async () => {
    const provider = await loadProvider();
    const config = provider.normalizeConfig({ apiKey: KEY, validateImported: false });
    expect(config.validateImported).toBe(true);
  });

  it("normalizes endpoints from raw host:port and URL forms", async () => {
    const provider = await loadProvider();
    expect(provider.normalizeConfig({ endpoint: "pr.example.com:9000" }).endpoint).toBe(
      "pr.example.com:9000"
    );
    expect(provider.normalizeConfig({ endpoint: "http://gw.proxyrise.com:443" }).endpoint).toBe(
      "gw.proxyrise.com:443"
    );
    expect(provider.normalizeConfig({ endpoint: "" }).endpoint).toBe("gw.proxyrise.com:443");
    expect(provider.normalizeConfig({ endpoint: "not a host" }).endpoint).toBe("gw.proxyrise.com:443");
  });

  it("rejects unknown types/countries back to safe fallbacks", async () => {
    const provider = await loadProvider();
    const config = provider.normalizeConfig({ type: "hacker", country: "usa" });
    expect(config.type).toBe("res");
    expect(config.country).toBe("any");
  });
});

describe("ProxyRise - fetch generates gateway entries", () => {
  it("requires an API key", async () => {
    const provider = await loadProvider();
    await expect(provider.fetch({})).rejects.toThrow(/API key/);
  });

  it("builds per-country sticky usernames deterministically", async () => {
    const provider = await loadProvider();
    const config = { apiKey: KEY, type: "res", country: "VG", perCountry: 3 };

    const first = await provider.fetch(config);
    const second = await provider.fetch(config);

    expect(first).toHaveLength(3);
    for (let i = 0; i < 3; i += 1) {
      expect(first[i].proxyUrl).toBe(second[i].proxyUrl);
      expect(first[i].username).toMatch(/^res-vg-sid-\d+$/);
      expect(first[i].proxyUrl).toContain(`:${encodeURIComponent(KEY)}@gw.proxyrise.com:443`);
      expect(first[i].country).toBe("VG");
    }
    expect(new Set(first.map((p) => p.username)).size).toBe(3);
  });

  it("omits the country segment for 'any'", async () => {
    const provider = await loadProvider();
    const proxies = await provider.fetch({ apiKey: KEY, country: "any", perCountry: 1 });
    expect(proxies[0].username).toMatch(/^res-sid-\d+$/);
  });

  it("reflects the chosen protocol in the proxy URL scheme", async () => {
    const provider = await loadProvider();
    const proxies = await provider.fetch({ apiKey: KEY, protocol: "https", perCountry: 1 });
    expect(proxies[0].proxyUrl.startsWith("https://")).toBe(true);
  });

  it("keeps session ids inside the documented range", async () => {
    const provider = await loadProvider();
    const proxies = await provider.fetch({ apiKey: KEY, country: "de", perCountry: 50 });
    const sids = proxies.map((p) => Number.parseInt(p.username.split("-sid-")[1], 10));
    expect(sids.every((sid) => sid >= 10000 && sid <= 999999999)).toBe(true);
  });
});

describe("ProxyRise - registration order", () => {
  it("is registered last so it renders last on the page", async () => {
    const providers = await getAllProviders();
    expect(providers.at(-1)?.id).toBe("proxyrise");
  });
});

describe("ProxyRise - refresh integration keeps only validated entries", () => {
  it("passes generated entries through validation before import", async () => {
    const result = await refreshProviderPool("proxyrise", {
      enabled: true,
      apiKey: KEY,
      country: "us",
      perCountry: 2,
    });

    expect(validateProxyList).toHaveBeenCalledTimes(1);
    expect(validateProxyList.mock.calls[0][0]).toHaveLength(2);
    expect(result.importedCount).toBe(2);
    expect(result.validationSummary.workingCount).toBe(2);
  });

  it("throws when nothing passes validation so dead entries never import", async () => {
    validateProxyList.mockImplementation(async (proxies) => ({
      checkedCount: proxies.length,
      workingCount: 0,
      failedCount: proxies.length,
      working: [],
      results: [],
    }));

    await expect(
      refreshProviderPool("proxyrise", { enabled: true, apiKey: KEY, country: "us" })
    ).rejects.toThrow(/passed validation/);
    expect(replaceProxyPoolsBySource).not.toHaveBeenCalled();
  });
});
