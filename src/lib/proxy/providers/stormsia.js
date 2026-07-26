import { registerProvider } from "./registry.js";
import { parseTextProxyList, FETCH_TIMEOUT_MS } from "../validateProxies.js";
import { originalFetch } from "../../../../open-sse/utils/proxyFetch.js";

const PROTOCOLS = ["http", "socks4", "socks5"];

registerProvider({
  id: "stormsia",
  name: "StormSia",
  description: "Free proxy list with auto-validation, updated regularly",
  defaults: {
    enabled: false,
    protocol: "socks5",
    limit: 500,
    validateImported: true,
    validationConcurrency: 10,
    minWorkingProxies: 1,
    intervalMinutes: 120,
  },
  configFields: [
    { name: "protocol", type: "select", label: "Protocol", options: PROTOCOLS },
    { name: "limit", type: "number", label: "Max proxies", min: 1, max: 5000 },
    { name: "validateImported", type: "boolean", label: "Validate before importing" },
    { name: "validationConcurrency", type: "number", label: "Validation concurrency", min: 1, max: 50 },
    { name: "minWorkingProxies", type: "number", label: "Min working proxies", min: 0, max: 5000 },
    { name: "intervalMinutes", type: "number", label: "Refresh interval (minutes)", min: 5, max: 10080 },
  ],
  normalizeConfig(input = {}) {
    return {
      enabled: input.enabled === true,
      protocol: PROTOCOLS.includes(input.protocol) ? input.protocol : "socks5",
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
    const url = `https://stormsia.github.io/proxy-list/list_${normalized.protocol}.txt`;

    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), FETCH_TIMEOUT_MS);
    let res;
    try {
      res = await originalFetch(url, { cache: "no-store", signal: controller.signal });
    } finally {
      clearTimeout(timer);
    }

    if (!res.ok) throw new Error(`StormSia returned ${res.status}`);

    const text = await res.text();
    return parseTextProxyList(text, normalized.protocol).slice(0, normalized.limit);
  },
});
