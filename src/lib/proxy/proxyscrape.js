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
  const normalized = normalizeProxyScrapeConfig(config || (await getSettings()).proxyscrapeProxyPool);
  const url = buildProxyScrapeUrl(normalized);

  // Use original native fetch to bypass env-proxy routing and add a timeout.
  // The patched global fetch may route through HTTP_PROXY/HTTPS_PROXY which
  // causes "fetch failed" when the proxy is down.
  const doFetch = fetchImpl || originalFetch;
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), FETCH_TIMEOUT_MS);
  let res;
  try {
    res = await doFetch(url, { cache: "no-store", signal: controller.signal });
  } catch (err) {
    const cause = err?.cause?.code || err?.name || "";
    const detail = cause ? ` (${cause})` : "";
    throw new Error(`ProxyScrape fetch failed${detail}: ${err?.message || err}`);
  } finally {
    clearTimeout(timer);
  }

  if (!res.ok) {
    throw new Error(`ProxyScrape returned ${res.status}`);
  }

  const text = await res.text();
  const proxies = parseProxyScrapeList(text, normalized.protocol).slice(0, normalized.limit);

  if (proxies.length === 0) {
    throw new Error("ProxyScrape returned no parseable proxies");
  }

  let validation = null;
  let validationSummary = null;
  let importProxies = proxies;

  if (normalized.validateImported) {
    validationSummary = await validateProxyList(proxies, normalized);
    if (validationSummary.workingCount < normalized.minWorkingProxies) {
      throw new Error(`Only ${validationSummary.workingCount} ProxyScrape proxies passed validation; minimum required is ${normalized.minWorkingProxies}`);
    }
    importProxies = validationSummary.working;
  }

  const pools = toPoolEntries(importProxies, normalized);
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
    lastCheckedCount: validationSummary?.checkedCount || (validation ? 1 : 0),
    lastWorkingCount: validationSummary?.workingCount || (validation?.ok ? 1 : 0),
  };

  await updateSettings({ proxyscrapeProxyPool: nextConfig });

  return {
    config: nextConfig,
    importedCount: replaced.length,
    firstProxy: proxies[0].proxyUrl,
    validation,
    validationSummary,
  };
}
