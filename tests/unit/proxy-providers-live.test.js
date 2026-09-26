import { describe, expect, it } from "vitest";
import { getAllProviders } from "@/lib/proxy/providers/registry.js";

/**
 * End-to-end smoke test: verify each provider's fetch() returns real proxy data.
 *
 * The live-fetch cases make real network calls, so they are gated behind
 * RUN_LIVE=1 the same way tests/e2e/real/* is gated behind RUN_REAL=1. A plain
 * `vitest run` therefore stays deterministic; public proxy lists rate-limit,
 * 404 and intermittently return empty results, which is not a code regression.
 *
 *   RUN_LIVE=1 npx vitest run unit/proxy-providers-live.test.js
 */
const RUN_LIVE = process.env.RUN_LIVE === "1";
const live = RUN_LIVE ? it : it.skip;

describe("Proxy providers — live fetch", () => {
  it("loads all providers from registry", async () => {
    const providers = await getAllProviders();
    expect(providers.length).toBeGreaterThanOrEqual(7);
  });

  live.each([
    "proxyscrape",
    "geonode",
    "thespeedx",
    "monosans",
    "stormsia",
  ])("%s returns real proxies", async (providerId) => {
    const { getProvider } = await import("@/lib/proxy/providers/registry.js");
    const provider = await getProvider(providerId);
    expect(provider).toBeTruthy();

    const config = provider.normalizeConfig({});
    const proxies = await provider.fetch(config);

    expect(Array.isArray(proxies)).toBe(true);
    expect(proxies.length).toBeGreaterThan(0);

    // Every proxy must have a valid proxyUrl
    for (const proxy of proxies) {
      expect(proxy.proxyUrl).toBeTruthy();
      expect(proxy.proxyUrl).toMatch(/^https?:\/\/.+|^(?:socks4|socks5):\/\//);
      expect(proxy.hostLabel).toBeTruthy();
    }

    // Show sample for debugging
    console.log(`    ${providerId}: ${proxies.length} proxies — sample: ${proxies[0].proxyUrl}`);
  }, 30000); // 30s timeout per provider for network calls
});

describe("Proxy providers — FreeProxy (requires API key)", () => {
  it("skips if no API key set", async () => {
    const { getProvider } = await import("@/lib/proxy/providers/registry.js");
    const provider = await getProvider("freeproxy");
    expect(provider).toBeTruthy();

    // FreeProxy needs an API key — skip gracefully
    const config = provider.normalizeConfig({ apiKey: "" });
    if (!config.apiKey) {
      console.log("    freeproxy: skipped (no API key)");
      return;
    }

    const proxies = await provider.fetch(config);
    expect(proxies.length).toBeGreaterThan(0);
  }, 30000);
});

describe("Proxy providers — Custom URL", () => {
  it("skips if no URL configured", async () => {
    const { getProvider } = await import("@/lib/proxy/providers/registry.js");
    const provider = await getProvider("custom");
    expect(provider).toBeTruthy();

    const config = provider.normalizeConfig({ url: "" });
    if (!config.url) {
      console.log("    custom: skipped (no URL)");
      return;
    }

    const proxies = await provider.fetch(config);
    expect(proxies.length).toBeGreaterThan(0);
  }, 30000);
});
