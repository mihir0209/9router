import { getProvider, toPoolEntries } from "./providers/registry.js";
import { validateProxyList } from "./validateProxies.js";
import { getLowUsageCountryCodes } from "./geoTargets.js";
import { replaceProxyPoolsBySource, getSettings, updateSettings } from "@/lib/localDb";

/**
 * Fetch proxies for a single provider, fanning out over low-usage target
 * countries when OpenCode geo targeting is enabled. Each country is fetched
 * independently (failure-isolated), results are deduped by URL and tagged
 * with their requested country when the source does not provide one.
 */
async function fetchProviderProxies(provider, normalized) {
  if (normalized.geoTargeting !== "opencode") {
    return provider.fetch(normalized);
  }

  const targets = await getLowUsageCountryCodes({ limit: normalized.geoMaxCountries });
  if (!targets.length) {
    throw new Error(
      "OpenCode geo targeting is enabled but no target countries are available yet " +
        "(run scripts/opencode-geo-sync.mjs or wait for the geo cache to populate)"
    );
  }

  const settled = await Promise.allSettled(
    targets.map((countryCode) => provider.fetch({ ...normalized, country: countryCode }))
  );

  const merged = [];
  const seenUrls = new Set();
  const failures = [];
  for (let i = 0; i < settled.length; i += 1) {
    const result = settled[i];
    const countryCode = targets[i];
    if (result.status === "rejected") {
      failures.push(`${countryCode}: ${result.reason?.message || result.reason}`);
      continue;
    }
    for (const proxy of result.value || []) {
      if (!proxy?.proxyUrl || seenUrls.has(proxy.proxyUrl)) continue;
      seenUrls.add(proxy.proxyUrl);
      merged.push({ ...proxy, country: proxy.country || countryCode });
    }
  }

  if (!merged.length) {
    throw new Error(
      `OpenCode geo fetch returned no proxies for any of ${targets.length} target countries` +
        (failures.length ? ` (${failures.join("; ")})` : "")
    );
  }

  return merged;
}

/**
 * Refresh proxies for a single provider.
 * @param {string} providerId - Provider ID
 * @param {object} config - Provider config
 * @returns {{ config, importedCount, firstProxy, validationSummary }}
 */
export async function refreshProviderPool(providerId, config) {
  const provider = await getProvider(providerId);
  if (!provider) throw new Error(`Unknown provider: ${providerId}`);

  const normalized = provider.normalizeConfig(config);

  // Fetch proxies from the provider
  const proxies = await fetchProviderProxies(provider, normalized);
  if (!proxies.length) throw new Error(`${provider.name} returned no parseable proxies`);

  let validationSummary = null;
  let importProxies = proxies;

  // Validate if enabled — always enforced in geo mode so only actively working
  // proxies are imported.
  const shouldValidate = normalized.geoTargeting === "opencode" ? true : normalized.validateImported;
  if (shouldValidate) {
    validationSummary = await validateProxyList(proxies, normalized);
    if (validationSummary.workingCount < (normalized.minWorkingProxies || 0)) {
      throw new Error(
        `Only ${validationSummary.workingCount} ${provider.name} proxies passed validation; ` +
        `minimum required is ${normalized.minWorkingProxies}`
      );
    }
    importProxies = validationSummary.working;
  }

  // Convert to pool entries
  const pools = toPoolEntries(importProxies, providerId, normalized);

  // Mark validated proxies with test results
  if (validationSummary) {
    for (let i = 0; i < pools.length; i += 1) {
      const item = importProxies[i];
      pools[i] = {
        ...pools[i],
        testStatus: "active",
        lastTestedAt: new Date().toISOString(),
        lastError: null,
        validationStatus: item.validation?.status || null,
        validationElapsedMs: item.validation?.elapsedMs || 0,
      };
    }
  }

  // Replace existing pools for this source
  const replaced = await replaceProxyPoolsBySource(providerId, pools);

  // Update provider config with refresh stats
  const nextConfig = {
    ...normalized,
    lastRefreshAt: new Date().toISOString(),
    lastError: null,
    lastImportedCount: replaced.length,
    lastCheckedCount: validationSummary?.checkedCount || 0,
    lastWorkingCount: validationSummary?.workingCount || 0,
  };

  await updateProviderConfig(providerId, nextConfig);

  return {
    config: nextConfig,
    importedCount: replaced.length,
    firstProxy: proxies[0]?.proxyUrl,
    validationSummary,
  };
}

/**
 * Refresh all enabled providers.
 */
export async function refreshAllEnabledPools() {
  const settings = await getSettings();
  const providers = settings.proxyPoolProviders || {};
  const results = [];

  for (const [providerId, config] of Object.entries(providers)) {
    if (!config.enabled) continue;
    try {
      const result = await refreshProviderPool(providerId, config);
      results.push({ providerId, ...result });
    } catch (err) {
      results.push({ providerId, error: err.message });
      // Update config with error
      await updateProviderConfig(providerId, {
        ...providers[providerId],
        lastError: err.message,
        lastRefreshAt: new Date().toISOString(),
      });
    }
  }

  return results;
}

/**
 * Get provider configs from settings.
 */
export async function getProviderConfigs() {
  const settings = await getSettings();
  return settings.proxyPoolProviders || {};
}

/**
 * Update a single provider's config in settings.
 */
export async function updateProviderConfig(providerId, config) {
  const settings = await getSettings();
  const providers = settings.proxyPoolProviders || {};
  providers[providerId] = config;
  await updateSettings({ proxyPoolProviders: providers });
}
