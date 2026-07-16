import { NextResponse } from "next/server";

const STATUS_CACHE_TTL_MS = 3000; // coalesce rapid polls; underlying probes already cache 10s

// Survive hot reload; one cache per process. Only tunnel/tailscale probes are cached —
// download progress stays live so the enable/download UI updates smoothly.
const statusCache = (global.__tunnelStatusCache ??= { value: null, fetchedAt: 0 });

export async function GET() {
  if (process.env.NINEROUTER_RUNTIME === "cloudflare") {
    return NextResponse.json({
      tunnel: { enabled: false, running: false, url: "", provider: "cloudflare", unsupported: true },
      tailscale: { enabled: false, running: false, url: "", unsupported: true },
      download: { active: false },
    });
  }

  try {
    const { getTunnelStatus, getTailscaleStatus, getDownloadStatus } = await import("@/lib/tunnel");
    let probes = statusCache.value;
    if (!probes || Date.now() - statusCache.fetchedAt >= STATUS_CACHE_TTL_MS) {
      const [tunnel, tailscale] = await Promise.all([getTunnelStatus(), getTailscaleStatus()]);
      probes = { tunnel, tailscale };
      statusCache.value = probes;
      statusCache.fetchedAt = Date.now();
    }
    const download = getDownloadStatus();
    return NextResponse.json({ ...probes, download });
  } catch (error) {
    console.error("Tunnel status error:", error);
    return NextResponse.json({ error: error.message }, { status: 500 });
  }
}
