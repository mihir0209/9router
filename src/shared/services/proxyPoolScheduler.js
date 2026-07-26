import { getSettings, updateSettings } from "@/lib/localDb";
import { getAllProviders, getProvider } from "@/lib/proxy/providers/registry.js";
import { refreshProviderPool, getProviderConfigs, updateProviderConfig } from "@/lib/proxy/refreshPool.js";

const MIN_TICK_MS = 60_000;

const g = global.__proxyPoolScheduler ??= {
  interval: null,
  running: false,
};

function isDue(config, now = Date.now()) {
  if (!config.enabled) return false;
  if (!config.lastRefreshAt) return true;
  const last = new Date(config.lastRefreshAt).getTime();
  if (!Number.isFinite(last)) return true;
  return now - last >= (config.intervalMinutes || 60) * 60_000;
}

export async function runProxyPoolSchedulerTick() {
  if (g.running) return { skipped: true, reason: "already-running" };
  g.running = true;

  const results = [];
  try {
    const configs = await getProviderConfigs();

    for (const [providerId, config] of Object.entries(configs)) {
      if (!config.enabled) continue;
      if (!isDue(config)) continue;

      try {
        const provider = await getProvider(providerId);
        if (!provider) continue;

        const result = await refreshProviderPool(providerId, config);
        console.log(`[${provider.name}] refreshed ${result.importedCount} proxies`);
        results.push({ providerId, ...result });
      } catch (error) {
        console.log(`[${providerId}] refresh failed:`, error?.message || error);
        results.push({ providerId, error: error?.message || String(error) });
        // Update config with error
        await updateProviderConfig(providerId, {
          ...config,
          lastError: error?.message || String(error),
          lastRefreshAt: new Date().toISOString(),
        });
      }
    }

    return { skipped: false, results };
  } catch (error) {
    console.log("[ProxyPool] scheduler tick failed:", error?.message || error);
    return { skipped: false, error: error?.message || String(error) };
  } finally {
    g.running = false;
  }
}

/**
 * Backward-compatible tick for the legacy ProxyScrape scheduler.
 * Delegates to the unified scheduler.
 */
export async function runProxyScrapeSchedulerTick() {
  return runProxyPoolSchedulerTick();
}

export function startProxyPoolScheduler() {
  if (g.interval) return;
  g.interval = setInterval(() => {
    runProxyPoolSchedulerTick().catch((error) => {
      console.log("[ProxyPool] scheduler tick failed:", error?.message || error);
    });
  }, MIN_TICK_MS);
  if (g.interval.unref) g.interval.unref();

  // Run once on startup
  runProxyPoolSchedulerTick().catch(() => {});
}

/**
 * Backward-compatible start for the legacy ProxyScrape scheduler.
 */
export function startProxyScrapeScheduler() {
  return startProxyPoolScheduler();
}
