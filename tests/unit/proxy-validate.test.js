import { describe, expect, it, vi, beforeEach } from "vitest";
import { parseTextProxyList } from "@/lib/proxy/validateProxies.js";

describe("validateProxies - parseTextProxyList", () => {
  it("parses plain host:port lines", () => {
    const result = parseTextProxyList("1.2.3.4:8080\n5.6.7.8:3128", "http");
    expect(result).toHaveLength(2);
    expect(result[0].proxyUrl).toContain("1.2.3.4:8080");
    expect(result[0].hostLabel).toBe("1.2.3.4:8080");
    expect(result[1].proxyUrl).toContain("5.6.7.8:3128");
    expect(result[1].hostLabel).toBe("5.6.7.8:3128");
  });

  it("parses URLs with protocol prefix", () => {
    const result = parseTextProxyList("socks5://1.2.3.4:1080\nhttp://5.6.7.8:8080", "http");
    expect(result).toHaveLength(2);
    expect(result[0].proxyUrl).toBe("socks5://1.2.3.4:1080");
    expect(result[0].protocol).toBe("socks5");
    expect(result[1].proxyUrl).toContain("5.6.7.8:8080");
    expect(result[1].protocol).toBe("http");
  });

  it("skips comments and empty lines", () => {
    const result = parseTextProxyList("# comment\n\n1.2.3.4:8080\n// another", "http");
    expect(result).toHaveLength(1);
    expect(result[0].proxyUrl).toContain("1.2.3.4:8080");
  });

  it("deduplicates proxies", () => {
    const result = parseTextProxyList("1.2.3.4:8080\n1.2.3.4:8080", "http");
    expect(result).toHaveLength(1);
  });

  it("skips malformed lines", () => {
    const result = parseTextProxyList("not-a-proxy\n:::invalid\n1.2.3.4:8080", "http");
    expect(result).toHaveLength(1);
  });

  it("handles empty input", () => {
    expect(parseTextProxyList("", "http")).toEqual([]);
    expect(parseTextProxyList(null, "http")).toEqual([]);
    expect(parseTextProxyList(undefined, "http")).toEqual([]);
  });

  it("applies default protocol when none specified", () => {
    const result = parseTextProxyList("1.2.3.4:8080", "socks5");
    expect(result[0].proxyUrl).toContain("socks5://");
    expect(result[0].protocol).toBe("socks5");
  });
});
