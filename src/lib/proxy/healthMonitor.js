import { getProxyPools, updateProxyPool, deleteProxyPool, getSettings } from "@/lib/localDb";
import { testProxyUrl } from "@/lib/network/proxyTest";

const DEFAULT_HEALTH_CHECK_INTERVAL_MS = 5 * 60 * 1000; // 5 minutes
const DEFAULT_MAX_CONSECUTIVE_FAILURES = 3;
const DEFAULT_DELETE_INACTIVE_AFTER_MS = 24 * 60 * 60 * 1000; // 24 hours

const g = global.__proxyHealthMonitor ??= {
  interval: null,
  running: false,
  stats: new Map(),
};

function log(message) {
  const ts = new Date().toISOString().slice(11, 19);
  console.log(`[ProxyHealth][${ts}] ${message}`);
}

export async function isHealthMonitorEnabled() {
  const settings = await getSettings();
  return settings.proxyHealthCheckEnabled !== false;
}

export async function getHealthCheckInterval() {
  const settings = await getSettings();
  return settings.proxyHealthCheckIntervalMs || DEFAULT_HEALTH_CHECK_INTERVAL_MS;
}

function getProxyStats(proxyId) {
  if (!g.stats.has(proxyId)) {
    g.stats.set(proxyId, {
      failures: 0, successes: 0,
      lastCheck: null, lastSuccess: null, lastFailure: null,
      avgLatency: 0, totalChecks: 0,
    });
  }
  return g.stats.get(proxyId);
}

function updateStats(proxyId, success, latencyMs) {
  const stats = getProxyStats(proxyId);
  stats.lastCheck = Date.now();
  stats.totalChecks++;

  if (success) {
    stats.successes++;
    stats.failures = 0;
    stats.lastSuccess = Date.now();
    stats.avgLatency = stats.totalChecks === 1
      ? latencyMs
      : stats.avgLatency * 0.7 + latencyMs * 0.3;
  } else {
    stats.failures++;
    stats.lastFailure = Date.now();
  }

  return stats;
}

async function checkProxy(proxy, timeoutMs = 8000) {
  const startedAt = Date.now();
  try {
    const result = await testProxyUrl({
      proxyUrl: proxy.proxyUrl,
      testUrl: "https://api.ipify.org/",
      method: "GET",
      timeoutMs,
    });
    return { ok: result.ok, latencyMs: Date.now() - startedAt, status: result.status, error: result.error };
  } catch (err) {
    return { ok: false, latencyMs: Date.now() - startedAt, status: 500, error: err.message };
  }
}

async function runHealthCheckForSource(source, proxies, timeoutMs) {
  const results = [];
  const batchSize = 10;

  for (let i = 0; i < proxies.length; i += batchSize) {
    const batch = proxies.slice(i, i + batchSize);
    const batchResults = await Promise.allSettled(
      batch.map(async (proxy) => {
        const check = await checkProxy(proxy, timeoutMs);
        const stats = updateStats(proxy.id, check.ok, check.latencyMs);

        if (stats.failures >= DEFAULT_MAX_CONSECUTIVE_FAILURES && proxy.isActive) {
          await updateProxyPool(proxy.id, {
            isActive: false,
            testStatus: "inactive",
            lastError: `Health check failed ${stats.failures}x consecutively`,
          });
          log(`Deactivated ${proxy.name || proxy.proxyUrl} after ${stats.failures} consecutive failures`);
        } else if (check.ok && !proxy.isActive) {
          await updateProxyPool(proxy.id, {
            isActive: true,
            testStatus: "active",
            lastError: null,
          });
          log(`Reactivated ${proxy.name || proxy.proxyUrl}`);
        }

        return { proxyId: proxy.id, ...check, stats };
      })
    );

    for (const settled of batchResults) {
      if (settled.status === "fulfilled") results.push(settled.value);
    }
  }

  return results;
}

async function cleanupDeadProxies(immediate = false) {
  const settings = await getSettings();
  const deleteAfterMs = settings.proxyDeleteInactiveAfterMs || DEFAULT_DELETE_INACTIVE_AFTER_MS;
  const now = Date.now();

  const inactivePools = await getProxyPools({ isActive: false });
  let deleted = 0;

  for (const pool of inactivePools) {
    const stats = g.stats.get(pool.id);

    if (immediate) {
      await deleteProxyPool(pool.id);
      g.stats.delete(pool.id);
      deleted++;
      continue;
    }

    const shouldDelete =
      !stats ||
      (stats.lastFailure && now - stats.lastFailure > deleteAfterMs && !stats.lastSuccess) ||
      (stats.lastCheck && now - stats.lastCheck > deleteAfterMs && stats.failures > 0);

    if (shouldDelete) {
      await deleteProxyPool(pool.id);
      g.stats.delete(pool.id);
      deleted++;
    }
  }

  if (deleted > 0) {
    log(`Cleaned up ${deleted} dead proxies${immediate ? " (immediate)" : ""}`);
  }

  return { deleted };
}

async function runHealthCheckTick() {
  if (g.running) return { skipped: true, reason: "already-running" };
  g.running = true;

  try {
    const settings = await getSettings();
    const timeoutMs = settings.proxyHealthCheckTimeoutMs || 8000;
    const sources = settings.proxyPoolProviders || {};

    const allPools = await getProxyPools({ isActive: true });
    const bySource = new Map();

    for (const pool of allPools) {
      const source = pool.source || "manual";
      if (!bySource.has(source)) bySource.set(source, []);
      bySource.get(source).push(pool);
    }

    const results = [];
    let checked = 0;
    let healthy = 0;
    let unhealthy = 0;

    for (const [source, proxies] of bySource) {
      const sourceConfig = sources[source];
      if (sourceConfig?.healthCheckEnabled === false) continue;

      log(`Checking ${proxies.length} proxies from ${source}...`);
      const sourceResults = await runHealthCheckForSource(source, proxies, timeoutMs);

      for (const r of sourceResults) {
        checked++;
        if (r.ok) healthy++;
        else unhealthy++;
      }

      results.push({ source, checked: sourceResults.length });
    }

    log(`Health check complete: ${checked} checked, ${healthy} healthy, ${unhealthy} unhealthy`);

    const cleanup = await cleanupDeadProxies();

    return { skipped: false, checked, healthy, unhealthy, results, deleted: cleanup.deleted };
  } catch (error) {
    log(`Health check failed: ${error?.message || error}`);
    return { skipped: false, error: error?.message || String(error) };
  } finally {
    g.running = false;
  }
}

export function startHealthMonitor(intervalMs = DEFAULT_HEALTH_CHECK_INTERVAL_MS) {
  if (g.interval) return;

  g.interval = setInterval(() => {
    runHealthCheckTick().catch((error) => {
      log(`Health check tick failed: ${error?.message || error}`);
    });
  }, intervalMs);

  if (g.interval.unref) g.interval.unref();
  log(`Health monitor started (interval: ${intervalMs / 1000}s)`);
}

export function stopHealthMonitor() {
  if (g.interval) {
    clearInterval(g.interval);
    g.interval = null;
    log("Health monitor stopped");
  }
}

export async function runImmediateHealthCheck() {
  return runHealthCheckTick();
}

export async function runImmediateCleanup(immediate = false) {
  return cleanupDeadProxies(immediate);
}

export function getHealthMonitorStatus() {
  return { running: g.running, hasInterval: !!g.interval, stats: Object.fromEntries(g.stats) };
}

export function getProxyHealthStats(proxyId) {
  return getProxyStats(proxyId);
}

export function resetProxyHealthStats(proxyId) {
  g.stats.delete(proxyId);
}
