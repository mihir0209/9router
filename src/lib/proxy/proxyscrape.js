import { createHash } from "crypto";
import { testProxyUrl } from "@/lib/network/proxyTest";
import { getSettings, replaceProxyPoolsBySource, updateSettings } from "@/lib/localDb";

export const PROXYSCRAPE_SOURCE = "proxyscrape";

export const DEFAULT_PROXYSCRAPE_CONFIG = {
  enabled: false,
  protocol: "socks5",
  country: "all",
  ssl: "all",
  anonymity: "all",
  timeoutMs: 10000,
  limit: 100,
  intervalMinutes: 60,
  lastRefreshAt: null,
  lastError: null,
  lastImportedCount: 0,
};

const VALID_PROTOCOLS = new Set(["all", "http", "socks4", "socks5"]);
const VALID_TRI_STATE = new Set(["all", "yes", "no"]);
const VALID_ANONYMITY = new Set(["all", "elite", "anonymous", "transparent"]);

function normalizeCountry(value) {
  const raw = typeof value === "string" ? value.trim() : "all";
  if (!raw || raw.toLowerCase() === "all") return "all";
  const upper = raw.toUpperCase();
  return /^[A-Z]{2}$/.test(upper) ? upper : "all";
}

function clampInt(value, fallback, min, max) {
  const n = Number.parseInt(value, 10);
  if (!Number.isFinite(n)) return fallback;
  return Math.min(Math.max(n, min), max);
}

function normalizeChoice(value, valid, fallback) {
  const normalized = typeof value === "string" ? value.trim().toLowerCase() : "";
  return valid.has(normalized) ? normalized : fallback;
}

export function normalizeProxyScrapeConfig(input = {}) {
  return {
    ...DEFAULT_PROXYSCRAPE_CONFIG,
    enabled: input.enabled === true,
    protocol: normalizeChoice(input.protocol, VALID_PROTOCOLS, DEFAULT_PROXYSCRAPE_CONFIG.protocol),
    country: normalizeCountry(input.country),
    ssl: normalizeChoice(input.ssl, VALID_TRI_STATE, DEFAULT_PROXYSCRAPE_CONFIG.ssl),
    anonymity: normalizeChoice(input.anonymity, VALID_ANONYMITY, DEFAULT_PROXYSCRAPE_CONFIG.anonymity),
    timeoutMs: clampInt(input.timeoutMs, DEFAULT_PROXYSCRAPE_CONFIG.timeoutMs, 1000, 60000),
    limit: clampInt(input.limit, DEFAULT_PROXYSCRAPE_CONFIG.limit, 1, 5000),
    intervalMinutes: clampInt(input.intervalMinutes, DEFAULT_PROXYSCRAPE_CONFIG.intervalMinutes, 5, 10080),
    lastRefreshAt: input.lastRefreshAt || null,
    lastError: input.lastError || null,
    lastImportedCount: clampInt(input.lastImportedCount, 0, 0, 5000),
  };
}

export function buildProxyScrapeUrl(config) {
  const normalized = normalizeProxyScrapeConfig(config);
  const params = new URLSearchParams({
    protocol: normalized.protocol,
    timeout: String(normalized.timeoutMs),
    country: normalized.country,
    ssl: normalized.ssl,
    anonymity: normalized.anonymity,
    limit: String(normalized.limit),
    request: "getproxies",
  });
  return `https://api.proxyscrape.com/v4/free-proxy-list/get?${params}`;
}

function schemeForProtocol(protocol) {
  if (protocol === "socks4" || protocol === "socks5") return protocol;
  return "http";
}

export function parseProxyScrapeList(text, protocol = "socks5") {
  const scheme = schemeForProtocol(protocol);
  const seen = new Set();
  const proxies = [];

  for (const line of String(text || "").split(/\r?\n/)) {
    const raw = line.trim();
    if (!raw || raw.startsWith("#")) continue;
    const proxyUrl = raw.includes("://") ? raw : `${scheme}://${raw}`;

    try {
      const parsed = new URL(proxyUrl);
      if (!parsed.hostname || !parsed.port) continue;
      const normalized = parsed.toString();
      if (seen.has(normalized)) continue;
      seen.add(normalized);
      proxies.push({ proxyUrl: normalized, hostLabel: `${parsed.hostname}:${parsed.port}` });
    } catch {
      // Skip malformed provider lines instead of failing the entire refresh.
    }
  }

  return proxies;
}

function idForProxy(proxyUrl) {
  const digest = createHash("sha256").update(proxyUrl).digest("hex").slice(0, 24);
  return `${PROXYSCRAPE_SOURCE}-${digest}`;
}

function toPoolEntries(proxies, config) {
  return proxies.map((proxy, index) => ({
    id: idForProxy(proxy.proxyUrl),
    name: `ProxyScrape ${proxy.hostLabel}`,
    proxyUrl: proxy.proxyUrl,
    noProxy: "",
    type: "http",
    strictProxy: false,
    isActive: true,
    testStatus: index === 0 ? "active" : "unknown",
    lastTestedAt: index === 0 ? new Date().toISOString() : null,
    lastError: null,
    source: PROXYSCRAPE_SOURCE,
    provider: "proxyscrape",
    proxyscrapeConfig: {
      protocol: config.protocol,
      country: config.country,
    },
  }));
}

export async function refreshProxyScrapePool({ config, validateFirst = true, fetchImpl = fetch } = {}) {
  const normalized = normalizeProxyScrapeConfig(config || (await getSettings()).proxyscrapeProxyPool);
  const url = buildProxyScrapeUrl(normalized);
  const res = await fetchImpl(url, { cache: "no-store" });

  if (!res.ok) {
    throw new Error(`ProxyScrape returned ${res.status}`);
  }

  const text = await res.text();
  const proxies = parseProxyScrapeList(text, normalized.protocol).slice(0, normalized.limit);

  if (proxies.length === 0) {
    throw new Error("ProxyScrape returned no parseable proxies");
  }

  let validation = null;
  if (validateFirst) {
    validation = await testProxyUrl({
      proxyUrl: proxies[0].proxyUrl,
      testUrl: "https://api.ipify.org/",
      method: "GET",
      timeoutMs: Math.min(normalized.timeoutMs, 15000),
    });
  }

  const pools = toPoolEntries(proxies, normalized);
  if (validation) {
    pools[0] = {
      ...pools[0],
      testStatus: validation.ok ? "active" : "unknown",
      lastTestedAt: new Date().toISOString(),
      lastError: validation.ok ? null : (validation.error || validation.statusText || `Proxy test failed with status ${validation.status}`),
    };
  }
  const replaced = await replaceProxyPoolsBySource(PROXYSCRAPE_SOURCE, pools);
  const nextConfig = {
    ...normalized,
    lastRefreshAt: new Date().toISOString(),
    lastError: validation && !validation.ok
      ? `Imported, but first proxy failed validation: ${validation.error || validation.statusText || validation.status}`
      : null,
    lastImportedCount: replaced.length,
  };

  await updateSettings({ proxyscrapeProxyPool: nextConfig });

  return {
    config: nextConfig,
    importedCount: replaced.length,
    firstProxy: proxies[0].proxyUrl,
    validation,
  };
}
