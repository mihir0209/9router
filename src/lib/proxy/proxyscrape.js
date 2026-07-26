import { createHash } from "crypto";
import { testProxyUrl } from "@/lib/network/proxyTest";
import { getSettings, replaceProxyPoolsBySource, updateSettings } from "@/lib/localDb";
import { originalFetch } from "../../../open-sse/utils/proxyFetch.js";

const FETCH_TIMEOUT_MS = 30000;

export const PROXYSCRAPE_SOURCE = "proxyscrape";

export const DEFAULT_PROXYSCRAPE_CONFIG = {
  enabled: false,
  protocol: "socks5",
  country: "all",
  ssl: "all",
  anonymity: "all",
  timeoutMs: 10000,
  limit: 100,
  validateImported: true,
  minWorkingProxies: 1,
  validationConcurrency: 10,
  intervalMinutes: 60,
  lastRefreshAt: null,
  lastError: null,
  lastImportedCount: 0,
  lastCheckedCount: 0,
  lastWorkingCount: 0,
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
    validateImported: input.validateImported !== false,
    minWorkingProxies: clampInt(input.minWorkingProxies, DEFAULT_PROXYSCRAPE_CONFIG.minWorkingProxies, 0, 5000),
    validationConcurrency: clampInt(input.validationConcurrency, DEFAULT_PROXYSCRAPE_CONFIG.validationConcurrency, 1, 50),
    intervalMinutes: clampInt(input.intervalMinutes, DEFAULT_PROXYSCRAPE_CONFIG.intervalMinutes, 5, 10080),
    lastRefreshAt: input.lastRefreshAt || null,
    lastError: input.lastError || null,
    lastImportedCount: clampInt(input.lastImportedCount, 0, 0, 5000),
    lastCheckedCount: clampInt(input.lastCheckedCount, 0, 0, 5000),
    lastWorkingCount: clampInt(input.lastWorkingCount, 0, 0, 5000),
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

async function validateProxyList(proxies, config) {
  const results = new Array(proxies.length);
  const queue = proxies.map((proxy, index) => ({ proxy, index }));

  const worker = async () => {
    while (queue.length > 0) {
      const item = queue.shift();
      if (!item) break;
      const validation = await testProxyUrl({
        proxyUrl: item.proxy.proxyUrl,
        testUrl: "https://api.ipify.org/",
        method: "GET",
        timeoutMs: Math.min(config.timeoutMs, 15000),
      });
      results[item.index] = {
        ...item.proxy,
        validation,
      };
    }
  };

  await Promise.all(Array.from({ length: Math.min(config.validationConcurrency, proxies.length) }, worker));

  const working = results.filter((entry) => entry?.validation?.ok);
  return {
    checkedCount: results.filter(Boolean).length,
    workingCount: working.length,
    failedCount: results.filter((entry) => entry && !entry.validation?.ok).length,
    working,
    results,
  };
}

export async function refreshProxyScrapePool({ config, fetchImpl } = {}) {
  // Delegate to the new provider system
  const { refreshProviderPool } = await import("./refreshPool.js");
  const settings = await getSettings();
  const mergedConfig = { ...normalizeProxyScrapeConfig(config || settings.proxyscrapeProxyPool) };
  
  try {
    const result = await refreshProviderPool("proxyscrape", mergedConfig);
    // Update legacy settings for backward compat
    await updateSettings({ proxyscrapeProxyPool: result.config });
    return result;
  } catch (error) {
    // Update legacy settings with error
    await updateSettings({
      proxyscrapeProxyPool: {
        ...mergedConfig,
        lastError: error.message,
        lastRefreshAt: new Date().toISOString(),
      },
    });
    throw error;
  }
}
