import { registerProvider } from "./registry.js";
import { parseTextProxyList, FETCH_TIMEOUT_MS } from "../validateProxies.js";
import { originalFetch } from "../../../../open-sse/utils/proxyFetch.js";

const PROTOCOLS = ["http", "https", "socks4", "socks5"];

registerProvider({
  id: "custom",
  name: "Custom URL",
  description: "Fetch proxies from any URL that returns ip:port or protocol://ip:port per line",
  defaults: {
    enabled: false,
    url: "",
    protocol: "http",
    authHeader: "",
    limit: 500,
    validateImported: true,
    validationConcurrency: 10,
    minWorkingProxies: 1,
    intervalMinutes: 120,
  },
  configFields: [
    { name: "url", type: "text", label: "Proxy list URL" },
    { name: "protocol", type: "select", label: "Default protocol (if not in URL)", options: PROTOCOLS },
    { name: "authHeader", type: "text", label: "Authorization header (optional)" },
    { name: "limit", type: "number", label: "Max proxies", min: 1, max: 5000 },
    { name: "validateImported", type: "boolean", label: "Validate before importing" },
    { name: "validationConcurrency", type: "number", label: "Validation concurrency", min: 1, max: 50 },
    { name: "minWorkingProxies", type: "number", label: "Min working proxies", min: 0, max: 5000 },
    { name: "intervalMinutes", type: "number", label: "Refresh interval (minutes)", min: 5, max: 10080 },
  ],
  normalizeConfig(input = {}) {
    return {
      enabled: input.enabled === true,
      url: typeof input.url === "string" ? input.url : "",
      protocol: PROTOCOLS.includes(input.protocol) ? input.protocol : "http",
      authHeader: typeof input.authHeader === "string" ? input.authHeader : "",
      limit: Number.isFinite(Number(input.limit)) ? Math.min(Math.max(Number(input.limit), 1), 5000) : 500,
      validateImported: input.validateImported !== false,
      validationConcurrency: Number.isFinite(Number(input.validationConcurrency)) ? Math.min(Math.max(Number(input.validationConcurrency), 1), 50) : 10,
      minWorkingProxies: Number.isFinite(Number(input.minWorkingProxies)) ? Math.min(Math.max(Number(input.minWorkingProxies), 0), 5000) : 1,
      intervalMinutes: Number.isFinite(Number(input.intervalMinutes)) ? Math.min(Math.max(Number(input.intervalMinutes), 5), 10080) : 120,
      lastRefreshAt: input.lastRefreshAt || null,
      lastError: input.lastError || null,
      lastImportedCount: Number(input.lastImportedCount) || 0,
      lastCheckedCount: Number(input.lastCheckedCount) || 0,
      lastWorkingCount: Number(input.lastWorkingCount) || 0,
    };
  },
  async fetch(config) {
    const normalized = this.normalizeConfig(config);
    if (!normalized.url) throw new Error("Custom provider requires a URL");

    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), FETCH_TIMEOUT_MS);
    const headers = { Accept: "text/plain, application/json" };
    if (normalized.authHeader) headers.Authorization = normalized.authHeader;

    let res;
    try {
      res = await originalFetch(normalized.url, {
        cache: "no-store",
        signal: controller.signal,
        headers,
      });
    } finally {
      clearTimeout(timer);
    }

    if (!res.ok) throw new Error(`Custom URL returned ${res.status}`);

    const contentType = res.headers.get("content-type") || "";
    const body = await res.text();

    if (contentType.includes("application/json")) {
      try {
        const data = JSON.parse(body);
        const proxies = [];
        const items = Array.isArray(data) ? data : data.data || data.proxies || [];
        for (const entry of items) {
          const ip = entry.ip || entry.host || entry.address;
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
      } catch {
        // Fall through to text parsing
      }
    }

    // Text format: ip:port or protocol://ip:port per line
    return parseTextProxyList(body, normalized.protocol).slice(0, normalized.limit);
  },
});
