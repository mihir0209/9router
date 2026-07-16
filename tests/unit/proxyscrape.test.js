import { describe, expect, it } from "vitest";
import {
  buildProxyScrapeUrl,
  normalizeProxyScrapeConfig,
  parseProxyScrapeList,
} from "@/lib/proxy/proxyscrape.js";

describe("ProxyScrape integration helpers", () => {
  it("normalizes config defaults and bounds", () => {
    const config = normalizeProxyScrapeConfig({
      enabled: true,
      protocol: "bad",
      country: "in",
      timeoutMs: 999999,
      limit: -5,
      intervalMinutes: 1,
    });

    expect(config.enabled).toBe(true);
    expect(config.protocol).toBe("socks5");
    expect(config.country).toBe("IN");
    expect(config.timeoutMs).toBe(60000);
    expect(config.limit).toBe(1);
    expect(config.intervalMinutes).toBe(5);
  });

  it("builds the ProxyScrape URL with user parameters", () => {
    const url = new URL(buildProxyScrapeUrl({
      protocol: "http",
      country: "US",
      timeoutMs: 5000,
      limit: 25,
      ssl: "yes",
      anonymity: "elite",
    }));

    expect(url.hostname).toBe("api.proxyscrape.com");
    expect(url.searchParams.get("protocol")).toBe("http");
    expect(url.searchParams.get("country")).toBe("US");
    expect(url.searchParams.get("timeout")).toBe("5000");
    expect(url.searchParams.get("limit")).toBe("25");
    expect(url.searchParams.get("ssl")).toBe("yes");
    expect(url.searchParams.get("anonymity")).toBe("elite");
    expect(url.searchParams.get("request")).toBe("getproxies");
  });

  it("parses plain host:port lines using the selected protocol", () => {
    const proxies = parseProxyScrapeList("1.2.3.4:1080\ninvalid\n1.2.3.4:1080\nhttp://5.6.7.8:8080", "socks5");

    expect(proxies).toEqual([
      { proxyUrl: "socks5://1.2.3.4:1080", hostLabel: "1.2.3.4:1080" },
      { proxyUrl: "http://5.6.7.8:8080/", hostLabel: "5.6.7.8:8080" },
    ]);
  });
});
