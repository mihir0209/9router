import { describe, expect, it, vi, beforeEach } from "vitest";

// Mock the localDb module
vi.mock("@/lib/localDb", () => ({
  getSettings: vi.fn(),
  updateSettings: vi.fn(),
}));

import { registerProvider, getProvider } from "@/lib/proxy/providers/registry.js";

describe("Custom URL Provider", () => {
  it("normalizes config with defaults", async () => {
    const provider = await getProvider("custom");
    expect(provider).toBeTruthy();

    const config = provider.normalizeConfig({
      url: "https://example.com/proxies.txt",
      protocol: "socks5",
    });

    expect(config.url).toBe("https://example.com/proxies.txt");
    expect(config.protocol).toBe("socks5");
    expect(config.validateImported).toBe(true);
  });

  it("defaults to http protocol", async () => {
    const provider = await getProvider("custom");
    const config = provider.normalizeConfig({});
    expect(config.protocol).toBe("http");
  });

  it("has config fields", async () => {
    const provider = await getProvider("custom");
    expect(provider.configFields.length).toBeGreaterThan(0);
    expect(provider.configFields.some(f => f.name === "url")).toBe(true);
    expect(provider.configFields.some(f => f.name === "protocol")).toBe(true);
  });
});

describe("TheSpeedX Provider", () => {
  it("normalizes config with defaults", async () => {
    const provider = await getProvider("thespeedx");
    expect(provider).toBeTruthy();

    const config = provider.normalizeConfig({});
    expect(config.protocol).toBe("http");
    expect(config.limit).toBe(500);
  });

  it("has config fields", async () => {
    const provider = await getProvider("thespeedx");
    expect(provider.configFields.length).toBeGreaterThan(0);
  });
});

describe("Monosans Provider", () => {
  it("normalizes config with defaults", async () => {
    const provider = await getProvider("monosans");
    expect(provider).toBeTruthy();

    const config = provider.normalizeConfig({});
    expect(config.protocol).toBe("http");
    expect(config.validateImported).toBe(true);
  });
});

describe("StormSia Provider", () => {
  it("normalizes config with defaults", async () => {
    const provider = await getProvider("stormsia");
    expect(provider).toBeTruthy();

    const config = provider.normalizeConfig({});
    expect(config.protocol).toBe("socks5");
    expect(config.validateImported).toBe(true);
  });
});

describe("GeoNode Provider", () => {
  it("normalizes config with defaults", async () => {
    const provider = await getProvider("geonode");
    expect(provider).toBeTruthy();

    const config = provider.normalizeConfig({});
    expect(config.protocols).toBe("http,https,socks4,socks5");
    expect(config.limit).toBe(100);
    expect(config.validateImported).toBe(true);
  });

  it("has config fields for country/protocol", async () => {
    const provider = await getProvider("geonode");
    expect(provider.configFields.some(f => f.name === "country")).toBe(true);
  });
});

describe("FreeProxy Provider", () => {
  it("normalizes config with defaults", async () => {
    const provider = await getProvider("freeproxy");
    expect(provider).toBeTruthy();

    const config = provider.normalizeConfig({});
    expect(config.protocol).toBe("http");
    expect(config.limit).toBe(100);
    expect(config.validateImported).toBe(true);
  });
});

describe("ProxyScrape Provider", () => {
  it("normalizes config with defaults", async () => {
    const provider = await getProvider("proxyscrape");
    expect(provider).toBeTruthy();

    const config = provider.normalizeConfig({});
    expect(config.protocol).toBe("socks5");
    expect(config.timeoutMs).toBe(10000);
    expect(config.limit).toBe(100);
  });
});
