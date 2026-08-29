import { registerProvider } from "./registry.js";
import { FETCH_TIMEOUT_MS } from "../validateProxies.js";
import { originalFetch } from "../../../../open-sse/utils/proxyFetch.js";

function clampInt(value, fallback, min, max) {
  const n = Number.parseInt(value, 10);
  if (!Number.isFinite(n)) return fallback;
  return Math.min(Math.max(n, min), max);
}

registerProvider({
  id: "freeproxy",
  name: "FreeProxy",
  description: "FreeProxy.com API with HTTP/HTTPS/SOCKS proxies, requires free API key",
  defaults: {
    enabled: false,
    apiKey: "",
    protocol: "http",
    country: "",
    limit: 100,
    validateImported: true,
    validationConcurrency: 10,
    minWorkingProxies: 1,
    intervalMinutes: 60,
    geoTargeting: "off",
    geoMaxCountries: 10,
  },
  configFields: [
    { name: "apiKey", type: "text", label: "API Key (get free at getfreeproxy.com)" },
    { name: "protocol", type: "select", label: "Protocol", options: ["http", "https", "socks4", "socks5"] },
    { name: "country", type: "text", label: "Country code (empty = all)" },
    {
      name: "geoTargeting",
      type: "select",
      label: "Geo targeting",
      options: ["off", "opencode"],
      help: "'opencode' fetches the least-used OpenCode countries (ascending token usage > threshold) instead of the fixed country above",
    },
    { name: "limit", type: "number", label: "Max proxies", min: 1, max: 5000 },
    { name: "validateImported", type: "boolean", label: "Validate before importing" },
    { name: "validationConcurrency", type: "number", label: "Validation concurrency", min: 1, max: 50 },
    { name: "minWorkingProxies", type: "number", label: "Min working proxies", min: 0, max: 5000 },
    { name: "intervalMinutes", type: "number", label: "Refresh interval (minutes)", min: 5, max: 10080 },
  ],
  normalizeConfig(input = {}) {
    return {
      enabled: input.enabled === true,
      apiKey: typeof input.apiKey === "string" ? input.apiKey : "",
      protocol: ["http", "https", "socks4", "socks5"].includes(input.protocol) ? input.protocol : "http",
      country: typeof input.country === "string" ? input.country : "",
      limit: clampInt(input.limit, 100, 1, 5000),
      validateImported: input.validateImported !== false,
      validationConcurrency: clampInt(input.validationConcurrency, 10, 1, 50),
      minWorkingProxies: clampInt(input.minWorkingProxies, 1, 0, 5000),
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
    if (!normalized.apiKey) throw new Error("FreeProxy requires an API key");

    const params = new URLSearchParams({
      protocol: normalized.protocol,
      page: "1",
    });
    if (normalized.country) params.set("country", normalized.country);

    const url = `https://api.getfreeproxy.com/v1/proxies?${params}`;

    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), FETCH_TIMEOUT_MS);
    let res;
    try {
      res = await originalFetch(url, {
        cache: "no-store",
        signal: controller.signal,
        headers: {
          Authorization: `Bearer ${normalized.apiKey}`,
          Accept: "application/json",
        },
      });
    } finally {
      clearTimeout(timer);
    }

    if (!res.ok) throw new Error(`FreeProxy returned ${res.status}`);

    const data = await res.json();
    const proxies = [];
    for (const entry of data.data || data.proxies || []) {
      const ip = entry.ip || entry.host;
      const port = entry.port;
      if (!ip || !port) continue;
      const protocol = (entry.protocol || normalized.protocol).toLowerCase();
      const proxyUrl = `${protocol}://${ip}:${port}`;
      proxies.push({
        proxyUrl,
        protocol,
        host: ip,
        port: String(port),
        hostLabel: `${ip}:${port}`,
        country: entry.country,
      });
    }
    return proxies.slice(0, normalized.limit);
  },
});
