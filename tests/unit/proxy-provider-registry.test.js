import { describe, expect, it, vi } from "vitest";
import {
  registerProvider,
  getProvider,
  getAllProviders,
  getProviderIds,
  generateProxyId,
  toPoolEntries,
} from "@/lib/proxy/providers/registry.js";

describe("Proxy Provider Registry", () => {
  it("registers and retrieves a provider", async () => {
    const mockProvider = {
      id: "test-provider",
      name: "Test Provider",
      description: "Test",
      fetch: async () => [],
      normalizeConfig: (c) => c,
      configFields: [],
    };

    registerProvider(mockProvider);
    const retrieved = await getProvider("test-provider");
    expect(retrieved?.name).toBe("Test Provider");
  });

  it("throws on invalid provider", () => {
    expect(() => registerProvider({})).toThrow("Provider must have id, name, and fetch");
    expect(() => registerProvider({ id: "x" })).toThrow("Provider must have id, name, and fetch");
  });

  it("generates deterministic proxy IDs", () => {
    const id1 = generateProxyId("test", "http://1.2.3.4:8080");
    const id2 = generateProxyId("test", "http://1.2.3.4:8080");
    const id3 = generateProxyId("other", "http://1.2.3.4:8080");

    expect(id1).toBe(id2);
    expect(id1).not.toBe(id3);
    expect(id1).toMatch(/^test-[a-f0-9]{24}$/);
  });

  it("converts proxies to pool entries", () => {
    const proxies = [
      { proxyUrl: "http://1.2.3.4:8080", hostLabel: "1.2.3.4:8080", country: "US" },
      { proxyUrl: "http://5.6.7.8:8080", hostLabel: "5.6.7.8:8080" },
    ];

    const entries = toPoolEntries(proxies, "test-source");

    expect(entries).toHaveLength(2);
    expect(entries[0].source).toBe("test-source");
    expect(entries[0].proxyUrl).toBe("http://1.2.3.4:8080");
    expect(entries[0].country).toBe("US");
    expect(entries[0].testStatus).toBe("active"); // first is marked active
    expect(entries[1].testStatus).toBe("unknown");
  });
});

describe("Provider Registry - built-in providers", () => {
  it("loads all built-in providers", async () => {
    const providers = await getAllProviders();
    const ids = await getProviderIds();

    expect(ids).toContain("proxyscrape");
    expect(ids).toContain("geonode");
    expect(ids).toContain("thespeedx");
    expect(ids).toContain("monosans");
    expect(ids).toContain("stormsia");
    expect(ids).toContain("freeproxy");
    expect(ids).toContain("custom");
    expect(providers.length).toBeGreaterThanOrEqual(7);
  });

  it("each built-in provider has required fields", async () => {
    const providers = await getAllProviders();

    for (const provider of providers) {
      expect(provider.id).toBeTruthy();
      expect(provider.name).toBeTruthy();
      expect(typeof provider.fetch).toBe("function");
      expect(typeof provider.normalizeConfig).toBe("function");
      expect(Array.isArray(provider.configFields)).toBe(true);
    }
  });
});
