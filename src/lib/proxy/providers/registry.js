import { createHash } from "crypto";

/**
 * @typedef {Object} ProxyProvider
 * @property {string} id - Unique provider identifier
 * @property {string} name - Display name
 * @property {string} description - Short description
 * @property {Object} defaults - Default config values
 * @property {Function} fetch - Fetch proxies: fetch(config) => [{proxyUrl, protocol, hostLabel}]
 * @property {Function} [normalizeConfig] - Normalize/validate config input
 * @property {Array<{name:string, type:string, label:string, options?:Array}>} configFields - Config UI schema
 */

/** @type {Map<string, ProxyProvider>} */
const registry = new Map();

let initPromise = null;

/**
 * Ensure all built-in providers are registered.
 * Uses dynamic imports to avoid circular dependencies.
 */
function ensureInitialized() {
  if (!initPromise) {
    initPromise = (async () => {
      await Promise.all([
        import("./proxyscrape.js"),
        import("./geonode.js"),
        import("./thespeedx.js"),
        import("./monosans.js"),
        import("./stormsia.js"),
        import("./freeproxy.js"),
        import("./custom.js"),
      ]);
      // Registered strictly after the free-list sources so ProxyRise renders last.
      await import("./proxyrise.js");
    })().catch(() => {});
  }
  return initPromise;
}

/**
 * Register a proxy provider.
 */
export function registerProvider(provider) {
  if (!provider?.id || !provider?.name || !provider?.fetch) {
    throw new Error("Provider must have id, name, and fetch");
  }
  registry.set(provider.id, provider);
}

/**
 * Get a registered provider by ID.
 */
export async function getProvider(id) {
  await ensureInitialized();
  return registry.get(id);
}

/**
 * Get all registered providers as an array.
 */
export async function getAllProviders() {
  await ensureInitialized();
  return Array.from(registry.values());
}

/**
 * Get provider IDs.
 */
export async function getProviderIds() {
  await ensureInitialized();
  return Array.from(registry.keys());
}

/**
 * Generate a deterministic ID for a proxy pool entry.
 */
export function generateProxyId(source, proxyUrl) {
  const digest = createHash("sha256").update(proxyUrl).digest("hex").slice(0, 24);
  return `${source}-${digest}`;
}

/**
 * Convert raw proxies to pool entries for database storage.
 */
export function toPoolEntries(proxies, source, config = {}) {
  return proxies.map((proxy, index) => ({
    id: generateProxyId(source, proxy.proxyUrl),
    name: `${source} ${proxy.hostLabel || proxy.proxyUrl}`,
    proxyUrl: proxy.proxyUrl,
    noProxy: "",
    type: "http",
    strictProxy: false,
    isActive: true,
    testStatus: index === 0 ? "active" : "unknown",
    lastTestedAt: index === 0 ? new Date().toISOString() : null,
    lastError: null,
    source,
    provider: source,
    ...(proxy.country && { country: proxy.country }),
    ...(proxy.latency && { latency: proxy.latency }),
  }));
}
