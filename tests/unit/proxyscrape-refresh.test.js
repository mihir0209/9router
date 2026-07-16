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

  it("keeps only proxies that pass validation", async () => {
    const fetchImpl = vi.fn().mockResolvedValue(textResponse("1.2.3.4:1080\n5.6.7.8:1080\n"));
    proxyTest.testProxyUrl
      .mockResolvedValueOnce({ ok: true, status: 200 })
      .mockResolvedValueOnce({ ok: false, error: "timeout" });

    const result = await refreshProxyScrapePool({
      config: { protocol: "socks5", limit: 2, validateImported: true, minWorkingProxies: 1 },
      fetchImpl,
    });

    expect(proxyTest.testProxyUrl).toHaveBeenCalledWith(expect.objectContaining({
      proxyUrl: "socks5://1.2.3.4:1080",
      testUrl: "https://api.ipify.org/",
      method: "GET",
    }));
    expect(localDb.replaceProxyPoolsBySource).toHaveBeenCalledWith("proxyscrape", [
      expect.objectContaining({ proxyUrl: "socks5://1.2.3.4:1080", source: "proxyscrape", testStatus: "active" }),
    ]);
    expect(result.importedCount).toBe(1);
    expect(result.validationSummary.workingCount).toBe(1);
  });

  it("does not replace rows when validated import has too few working proxies", async () => {
    const fetchImpl = vi.fn().mockResolvedValue(textResponse("1.2.3.4:1080\n"));
    proxyTest.testProxyUrl.mockResolvedValue({ ok: false, error: "timeout" });

    await expect(refreshProxyScrapePool({
      config: { protocol: "socks5", limit: 1, validateImported: true, minWorkingProxies: 1 },
      fetchImpl,
    })).rejects.toThrow("Only 0 ProxyScrape proxies passed validation");

    expect(localDb.replaceProxyPoolsBySource).not.toHaveBeenCalled();
    expect(localDb.updateSettings).not.toHaveBeenCalled();
  });

  it("can import directly when validation filtering is disabled", async () => {
    const fetchImpl = vi.fn().mockResolvedValue(textResponse("1.2.3.4:1080\n"));
    proxyTest.testProxyUrl.mockResolvedValue({ ok: false, error: "timeout" });

    const result = await refreshProxyScrapePool({
      config: { protocol: "socks5", limit: 1, validateImported: false },
      fetchImpl,
    });

    expect(localDb.replaceProxyPoolsBySource).toHaveBeenCalledWith("proxyscrape", expect.arrayContaining([
      expect.objectContaining({ proxyUrl: "socks5://1.2.3.4:1080", source: "proxyscrape" }),
    ]));
    expect(proxyTest.testProxyUrl).not.toHaveBeenCalled();
    expect(result.importedCount).toBe(1);
  });
});
