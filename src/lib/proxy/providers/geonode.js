import { registerProvider } from "./registry.js";
import { FETCH_TIMEOUT_MS } from "../validateProxies.js";
import { originalFetch } from "../../../../open-sse/utils/proxyFetch.js";

function clampInt(value, fallback, min, max) {
  const n = Number.parseInt(value, 10);
  if (!Number.isFinite(n)) return fallback;
  return Math.min(Math.max(n, min), max);
}

registerProvider({
  id: "geonode",
  name: "GeoNode",
  description: "Free proxy list from GeoNode with 5,000+ proxies across 100+ countries",
  defaults: {
    enabled: false,
    protocols: "http,https,socks4,socks5",
    country: "",
    anonymity: "",
    speed: "",
    limit: 100,
    validateImported: true,
    validationConcurrency: 10,
    minWorkingProxies: 1,
    intervalMinutes: 60,
  },
  configFields: [
    { name: "protocols", type: "text", label: "Protocols (comma-separated: http,https,socks4,socks5)" },
    { name: "country", type: "text", label: "Country code (empty = all)" },
    { name: "anonymity", type: "select", label: "Anonymity", options: ["", "elite", "anonymous", "transparent"] },
    { name: "speed", type: "select", label: "Speed", options: ["", "fast", "medium", "slow"] },
    { name: "limit", type: "number", label: "Max proxies", min: 1, max: 5000 },
    { name: "validateImported", type: "boolean", label: "Validate before importing" },
    { name: "validationConcurrency", type: "number", label: "Validation concurrency", min: 1, max: 50 },
    { name: "minWorkingProxies", type: "number", label: "Min working proxies", min: 0, max: 5000 },
    { name: "intervalMinutes", type: "number", label: "Refresh interval (minutes)", min: 5, max: 10080 },
  ],
  normalizeConfig(input = {}) {
    return {
      enabled: input.enabled === true,
      protocols: typeof input.protocols === "string" ? input.protocols : "http,https,socks4,socks5",
      country: typeof input.country === "string" ? input.country : "",
      anonymity: typeof input.anonymity === "string" ? input.anonymity : "",
      speed: typeof input.speed === "string" ? input.speed : "",
      limit: clampInt(input.limit, 100, 1, 5000),
      validateImported: input.validateImported !== false,
      validationConcurrency: clampInt(input.validationConcurrency, 10, 1, 50),
      minWorkingProxies: clampInt(input.minWorkingProxies, 1, 0, 5000),
      intervalMinutes: clampInt(input.intervalMinutes, 60, 5, 10080),
      lastRefreshAt: input.lastRefreshAt || null,
      lastError: input.lastError || null,
      lastImportedCount: clampInt(input.lastImportedCount, 0, 0, 5000),
      lastCheckedCount: clampInt(input.lastCheckedCount, 0, 0, 5000),
      lastWorkingCount: clampInt(input.lastWorkingCount, 0, 0, 5000),
    };
  },
  async fetch(config) {
    const normalized = this.normalizeConfig(config);
    const params = new URLSearchParams({
      limit: String(normalized.limit),
      sort_by: "lastChecked",
      sort_type: "desc",
    });
    if (normalized.protocols) params.set("protocols", normalized.protocols);
    if (normalized.country) params.set("country", normalized.country);
    if (normalized.anonymity) params.set("anonymity", normalized.anonymity);
    if (normalized.speed) params.set("speed", normalized.speed);

    const url = `https://proxylist.geonode.com/api/proxy-list?${params}`;

    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), FETCH_TIMEOUT_MS);
    let res;
    try {
      res = await originalFetch(url, {
        cache: "no-store",
        signal: controller.signal,
        headers: { Accept: "application/json" },
      });
    } finally {
      clearTimeout(timer);
    }

    if (!res.ok) throw new Error(`GeoNode returned ${res.status}`);

    const data = await res.json();
    const proxies = [];
    for (const entry of data.data || []) {
      const protocol = (entry.protocols?.[0] || "http").toLowerCase();
      const proxyUrl = `${protocol}://${entry.ip}:${entry.port}`;
      proxies.push({
        proxyUrl,
        protocol,
        host: entry.ip,
        port: String(entry.port),
        hostLabel: `${entry.ip}:${entry.port}`,
        country: entry.country,
        latency: entry.responseTime,
      });
    }
    return proxies.slice(0, normalized.limit);
  },
});
