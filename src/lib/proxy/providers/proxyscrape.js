import { registerProvider } from "./registry.js";
import { parseTextProxyList, FETCH_TIMEOUT_MS } from "../validateProxies.js";
import { originalFetch } from "../../../../open-sse/utils/proxyFetch.js";

const VALID_PROTOCOLS = ["all", "http", "socks4", "socks5"];
const VALID_TRI_STATE = ["all", "yes", "no"];
const VALID_ANONYMITY = ["all", "elite", "anonymous", "transparent"];

function normalizeChoice(value, valid, fallback) {
  const normalized = typeof value === "string" ? value.trim().toLowerCase() : "";
  return valid.includes(normalized) ? normalized : fallback;
}

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

function buildUrl(config) {
  const params = new URLSearchParams({
    protocol: config.protocol,
    timeout: String(config.timeoutMs),
    country: config.country,
    ssl: config.ssl,
    anonymity: config.anonymity,
    limit: String(config.limit),
    request: "getproxies",
  });
  return `https://api.proxyscrape.com/v4/free-proxy-list/get?${params}`;
}

registerProvider({
  id: "proxyscrape",
  name: "ProxyScrape",
  description: "Free HTTP, HTTPS, SOCKS4 & SOCKS5 proxies from 180+ countries, refreshed every minute",
  defaults: {
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
    geoTargeting: "off",
    geoMaxCountries: 10,
  },
  configFields: [
    { name: "protocol", type: "select", label: "Protocol", options: ["all", "http", "socks4", "socks5"] },
    { name: "country", type: "text", label: "Country (ISO code or 'all')" },
    {
      name: "geoTargeting",
      type: "select",
      label: "Geo targeting",
      options: ["off", "opencode"],
      help: "'opencode' fetches the least-used OpenCode countries (ascending token usage > threshold) instead of the fixed country above",
    },
    { name: "ssl", type: "select", label: "SSL", options: ["all", "yes", "no"] },
    { name: "anonymity", type: "select", label: "Anonymity", options: ["all", "elite", "anonymous", "transparent"] },
    { name: "timeoutMs", type: "number", label: "Timeout (ms)", min: 1000, max: 60000 },
    { name: "limit", type: "number", label: "Max proxies", min: 1, max: 5000 },
    { name: "validateImported", type: "boolean", label: "Validate before importing" },
    { name: "validationConcurrency", type: "number", label: "Validation concurrency", min: 1, max: 50 },
    { name: "minWorkingProxies", type: "number", label: "Min working proxies", min: 0, max: 5000 },
    { name: "intervalMinutes", type: "number", label: "Refresh interval (minutes)", min: 5, max: 10080 },
  ],
  normalizeConfig(input = {}) {
    return {
      enabled: input.enabled === true,
      protocol: normalizeChoice(input.protocol, VALID_PROTOCOLS, "socks5"),
      country: normalizeCountry(input.country),
      ssl: normalizeChoice(input.ssl, VALID_TRI_STATE, "all"),
      anonymity: normalizeChoice(input.anonymity, VALID_ANONYMITY, "all"),
      timeoutMs: clampInt(input.timeoutMs, 10000, 1000, 60000),
      limit: clampInt(input.limit, 100, 1, 5000),
      validateImported: input.validateImported !== false,
      minWorkingProxies: clampInt(input.minWorkingProxies, 1, 0, 5000),
      validationConcurrency: clampInt(input.validationConcurrency, 10, 1, 50),
      intervalMinutes: clampInt(input.intervalMinutes, 60, 5, 10080),
      geoTargeting: input.geoTargeting === "opencode" ? "opencode" : "off",
      geoMaxCountries: clampInt(input.geoMaxCountries, 10, 1, 40),
      lastRefreshAt: input.lastRefreshAt || null,
      lastError: input.lastError || null,
      lastImportedCount: clampInt(input.lastImportedCount, 0, 0, 5000),
      lastCheckedCount: clampInt(input.lastCheckedCount, 0, 0, 5000),
      lastWorkingCount: clampInt(input.lastWorkingCount, 0, 0, 5000),
    };
  },
  async fetch(config) {
    const normalized = this.normalizeConfig(config);
    const url = buildUrl(normalized);

    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), FETCH_TIMEOUT_MS);
    let res;
    try {
      res = await originalFetch(url, { cache: "no-store", signal: controller.signal });
    } finally {
      clearTimeout(timer);
    }

    if (!res.ok) throw new Error(`ProxyScrape returned ${res.status}`);

    const text = await res.text();
    return parseTextProxyList(text, normalized.protocol === "all" ? "http" : normalized.protocol)
      .slice(0, normalized.limit);
  },
});
