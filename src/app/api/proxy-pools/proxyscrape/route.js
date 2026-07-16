import { NextResponse } from "next/server";
import { getSettings, updateSettings } from "@/lib/localDb";
import { normalizeProxyScrapeConfig } from "@/lib/proxy/proxyscrape";

export const dynamic = "force-dynamic";
export const revalidate = 0;

export async function GET() {
  try {
    const settings = await getSettings();
    return NextResponse.json({ config: normalizeProxyScrapeConfig(settings.proxyscrapeProxyPool) });
  } catch (error) {
    console.log("Error fetching ProxyScrape settings:", error);
    return NextResponse.json({ error: "Failed to fetch ProxyScrape settings" }, { status: 500 });
  }
}

export async function PUT(request) {
  try {
    const body = await request.json();
    const settings = await getSettings();
    const current = normalizeProxyScrapeConfig(settings.proxyscrapeProxyPool);
    const next = normalizeProxyScrapeConfig({ ...current, ...body });
    const updated = await updateSettings({ proxyscrapeProxyPool: next });

    return NextResponse.json({ config: normalizeProxyScrapeConfig(updated.proxyscrapeProxyPool) });
  } catch (error) {
    console.log("Error updating ProxyScrape settings:", error);
    return NextResponse.json({ error: "Failed to update ProxyScrape settings" }, { status: 500 });
  }
}
