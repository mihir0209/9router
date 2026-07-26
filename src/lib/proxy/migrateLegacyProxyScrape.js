import { getSettings, updateSettings } from "@/lib/localDb";

/**
 * Migrate legacy `proxyscrapeProxyPool` setting into the unified
 * `proxyPoolProviders.proxyscrape` config structure.
 *
 * Idempotent — safe to call on every startup.
 * Only runs if `proxyPoolProviders.proxyscrape` does NOT already exist
 * AND `proxyscrapeProxyPool` is non-empty.
 */
export async function migrateLegacyProxyScrape() {
  const settings = await getSettings();

  const legacyConfig = settings.proxyscrapeProxyPool;
  const providers = settings.proxyPoolProviders || {};

  // Already migrated or no legacy config to migrate
  if (providers.proxyscrape || !legacyConfig) return false;

  providers.proxyscrape = { ...legacyConfig };

  await updateSettings({ proxyPoolProviders: providers });
  return true;
}
