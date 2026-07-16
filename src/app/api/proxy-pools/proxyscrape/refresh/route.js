import { NextResponse } from "next/server";
import { getSettings, updateSettings } from "@/lib/localDb";
import { normalizeProxyScrapeConfig, refreshProxyScrapePool } from "@/lib/proxy/proxyscrape";

export const dynamic = "force-dynamic";
export const revalidate = 0;

export async function POST() {
  try {
    const result = await refreshProxyScrapePool({ validateFirst: true });
    return NextResponse.json(result);
  } catch (error) {
    console.log("Error refreshing ProxyScrape proxies:", error);
    try {
      const settings = await getSettings();
      const current = normalizeProxyScrapeConfig(settings.proxyscrapeProxyPool);
      await updateSettings({
        proxyscrapeProxyPool: {
          ...current,
          lastError: error?.message || "ProxyScrape refresh failed",
        },
      });
    } catch {
      // Preserve the original refresh failure response.
    }
    return NextResponse.json({ error: error?.message || "ProxyScrape refresh failed" }, { status: 502 });
  }
}
