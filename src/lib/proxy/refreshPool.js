import { getProvider, toPoolEntries } from "./providers/registry.js";
import { validateProxyList } from "./validateProxies.js";
import { replaceProxyPoolsBySource, getSettings, updateSettings } from "@/lib/localDb";

/**
 * Refresh proxies for a single provider.
 * @param {string} providerId - Provider ID
 * @param {object} config - Provider config
 * @returns {{ config, importedCount, firstProxy, validationSummary }}
 */
export async function refreshProviderPool(providerId, config) {
  const provider = getProvider(providerId);
  if (!provider) throw new Error(`Unknown provider: ${providerId}`);

  const normalized = provider.normalizeConfig(config);

  // Fetch proxies from the provider
  const proxies = await provider.fetch(normalized);
  if (!proxies.length) throw new Error(`${provider.name} returned no parseable proxies`);

  let validationSummary = null;
  let importProxies = proxies;

  // Validate if enabled
  if (normalized.validateImported) {
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
