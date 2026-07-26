import { describe, expect, it, vi, beforeEach } from "vitest";

// Mock the localDb module
vi.mock("@/lib/localDb", () => ({
  getSettings: vi.fn(),
  updateSettings: vi.fn(),
}));

import { getSettings, updateSettings } from "@/lib/localDb";
import { migrateLegacyProxyScrape } from "@/lib/proxy/migrateLegacyProxyScrape.js";

describe("migrateLegacyProxyScrape", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("migrates legacy config to unified provider system", async () => {
    const legacyConfig = {
      enabled: true,
      protocol: "socks5",
      country: "US",
      timeoutMs: 5000,
      limit: 100,
    };

    getSettings.mockResolvedValue({
      proxyscrapeProxyPool: legacyConfig,
      proxyPoolProviders: {},
    });

    const migrated = await migrateLegacyProxyScrape();

    expect(migrated).toBe(true);
    expect(updateSettings).toHaveBeenCalledWith({
      proxyPoolProviders: {
        proxyscrape: legacyConfig,
      },
    });
  });

  it("does not overwrite existing provider config", async () => {
    const existingConfig = { enabled: true, protocol: "http" };
    const legacyConfig = { enabled: true, protocol: "socks5" };

    getSettings.mockResolvedValue({
      proxyscrapeProxyPool: legacyConfig,
      proxyPoolProviders: { proxyscrape: existingConfig },
    });

    const migrated = await migrateLegacyProxyScrape();

    expect(migrated).toBe(false);
    expect(updateSettings).not.toHaveBeenCalled();
  });

  it("does nothing when no legacy config exists", async () => {
    getSettings.mockResolvedValue({
      proxyscrapeProxyPool: null,
      proxyPoolProviders: {},
    });

    const migrated = await migrateLegacyProxyScrape();

    expect(migrated).toBe(false);
    expect(updateSettings).not.toHaveBeenCalled();
  });

  it("handles missing proxyPoolProviders gracefully", async () => {
    getSettings.mockResolvedValue({
      proxyscrapeProxyPool: { enabled: true },
    });

    const migrated = await migrateLegacyProxyScrape();

    expect(migrated).toBe(true);
    expect(updateSettings).toHaveBeenCalledWith({
      proxyPoolProviders: {
        proxyscrape: { enabled: true },
      },
    });
  });
});
