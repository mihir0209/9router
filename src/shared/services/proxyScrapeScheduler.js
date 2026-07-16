import { getSettings, updateSettings } from "@/lib/localDb";
import { normalizeProxyScrapeConfig, refreshProxyScrapePool } from "@/lib/proxy/proxyscrape";

const MIN_TICK_MS = 60_000;

const g = global.__proxyscrapeScheduler ??= {
  interval: null,
  running: false,
};

function isDue(config, now = Date.now()) {
  if (!config.enabled) return false;
  if (!config.lastRefreshAt) return true;
  const last = new Date(config.lastRefreshAt).getTime();
  if (!Number.isFinite(last)) return true;
  return now - last >= config.intervalMinutes * 60_000;
}

export async function runProxyScrapeSchedulerTick() {
  if (g.running) return { skipped: true, reason: "already-running" };
  g.running = true;
  try {
    const settings = await getSettings();
    const config = normalizeProxyScrapeConfig(settings.proxyscrapeProxyPool);
    if (!isDue(config)) return { skipped: true, reason: "not-due", config };

    const result = await refreshProxyScrapePool({ config, validateFirst: true });
    console.log(`[ProxyScrape] refreshed ${result.importedCount} proxies`);
    return { skipped: false, result };
  } catch (error) {
    try {
      const settings = await getSettings();
      const config = normalizeProxyScrapeConfig(settings.proxyscrapeProxyPool);
      if (config.enabled) {
        await updateSettings({
          proxyscrapeProxyPool: {
            ...config,
            lastError: error?.message || String(error),
          },
        });
      }
    } catch {
      // Keep scheduler fail-open.
    }
    console.log("[ProxyScrape] scheduled refresh failed:", error?.message || error);
    return { skipped: false, error: error?.message || String(error) };
  } finally {
    g.running = false;
  }
}

export function startProxyScrapeScheduler() {
  if (g.interval) return;
  g.interval = setInterval(() => {
    runProxyScrapeSchedulerTick().catch((error) => {
      console.log("[ProxyScrape] scheduler tick failed:", error?.message || error);
    });
  }, MIN_TICK_MS);
  if (g.interval.unref) g.interval.unref();

  runProxyScrapeSchedulerTick().catch(() => {});
}
