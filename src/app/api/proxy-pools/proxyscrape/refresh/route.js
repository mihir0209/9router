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
    const cause = error?.cause?.code || error?.cause || "";
    const fullMessage = cause
      ? `${error?.message || "ProxyScrape refresh failed"} [cause: ${cause}]`
      : error?.message || "ProxyScrape refresh failed";
    try {
      const settings = await getSettings();
      const current = normalizeProxyScrapeConfig(settings.proxyscrapeProxyPool);
      await updateSettings({
        proxyscrapeProxyPool: {
          ...current,
          lastError: fullMessage,
        },
      });
    } catch {
      // Preserve the original refresh failure response.
    }
    return NextResponse.json({ error: fullMessage }, { status: 502 });
  }
}
