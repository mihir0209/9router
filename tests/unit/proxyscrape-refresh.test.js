import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@/lib/localDb", () => ({
  getSettings: vi.fn(),
  replaceProxyPoolsBySource: vi.fn(),
  updateSettings: vi.fn(),
}));

vi.mock("@/lib/network/proxyTest", () => ({
  testProxyUrl: vi.fn(),
}));

const localDb = await import("@/lib/localDb");
const proxyTest = await import("@/lib/network/proxyTest");
const { refreshProxyScrapePool } = await import("@/lib/proxy/proxyscrape.js");

function textResponse(body, ok = true, status = 200) {
  return {
    ok,
    status,
    text: async () => body,
  };
}

describe("ProxyScrape refresh", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    localDb.replaceProxyPoolsBySource.mockImplementation(async (_source, pools) => pools);
    localDb.updateSettings.mockImplementation(async (updates) => updates);
    proxyTest.testProxyUrl.mockResolvedValue({ ok: true, status: 200 });
  });

  it("validates first proxy before replacing managed rows", async () => {
    const fetchImpl = vi.fn().mockResolvedValue(textResponse("1.2.3.4:1080\n5.6.7.8:1080\n"));

    const result = await refreshProxyScrapePool({
      config: { protocol: "socks5", limit: 2 },
      fetchImpl,
    });

    expect(proxyTest.testProxyUrl).toHaveBeenCalledWith(expect.objectContaining({
      proxyUrl: "socks5://1.2.3.4:1080",
      testUrl: "https://api.ipify.org/",
      method: "GET",
    }));
    expect(localDb.replaceProxyPoolsBySource).toHaveBeenCalledWith("proxyscrape", expect.arrayContaining([
      expect.objectContaining({ proxyUrl: "socks5://1.2.3.4:1080", source: "proxyscrape" }),
      expect.objectContaining({ proxyUrl: "socks5://5.6.7.8:1080", source: "proxyscrape" }),
    ]));
    expect(result.importedCount).toBe(2);
  });

  it("still replaces rows when first proxy validation fails", async () => {
    const fetchImpl = vi.fn().mockResolvedValue(textResponse("1.2.3.4:1080\n"));
    proxyTest.testProxyUrl.mockResolvedValue({ ok: false, error: "timeout" });

    const result = await refreshProxyScrapePool({
      config: { protocol: "socks5", limit: 1 },
      fetchImpl,
    });

    expect(localDb.replaceProxyPoolsBySource).toHaveBeenCalledWith("proxyscrape", [
      expect.objectContaining({
        proxyUrl: "socks5://1.2.3.4:1080",
        source: "proxyscrape",
        testStatus: "unknown",
        lastError: "timeout",
      }),
    ]);
    expect(localDb.updateSettings).toHaveBeenCalledWith({
      proxyscrapeProxyPool: expect.objectContaining({
        lastError: "Imported, but first proxy failed validation: timeout",
        lastImportedCount: 1,
      }),
    });
    expect(result.importedCount).toBe(1);
  });
});
