import { createHash } from "crypto";
import { registerProvider } from "./registry.js";

const VALID_TYPES = ["res", "res_sc", "res_ls", "stc", "stc_sc", "mob", "dc"];
const VALID_PROTOCOLS = ["http", "https"];
const DEFAULT_ENDPOINT = "gw.proxyrise.com:443";
const SID_MIN = 10000;
const SID_MAX = 999999999;

function clampInt(value, fallback, min, max) {
  const n = Number.parseInt(value, 10);
  if (!Number.isFinite(n)) return fallback;
  return Math.min(Math.max(n, min), max);
}

function normalizeCountry(value) {
  const raw = typeof value === "string" ? value.trim().toLowerCase() : "";
  if (!raw || raw === "any") return "any";
  return /^[a-z]{2}$/.test(raw) ? raw : "any";
}

function normalizeEndpoint(value) {
  const raw = typeof value === "string" ? value.trim() : "";
  if (!raw) return DEFAULT_ENDPOINT;
  if (/^[a-z0-9.-]+:\d+$/i.test(raw)) return raw.toLowerCase();
  if (/^https?:\/\//i.test(raw)) {
    try {
      const parsed = new URL(raw);
      if (parsed.hostname && parsed.port) return `${parsed.hostname}:${parsed.port}`;
    } catch {}
  }
  return DEFAULT_ENDPOINT;
}

/**
 * Deterministic sticky-session id: same key/type/country/slot always maps to
 * the same proxy string, so refreshes upsert identical pool entries instead of
 * churning paid bandwidth.
 */
function stableSessionId(apiKey, type, country, slot) {
  const digest = createHash("sha256").update(`${apiKey}:${type}:${country}:${slot}`).digest("hex");
  const num = Number.parseInt(digest.slice(0, 12), 16);
  return SID_MIN + (num % (SID_MAX - SID_MIN));
}

registerProvider({
  id: "proxyrise",
  name: "ProxyRise",
  description:
    "Paid gateway (res/stc/mob/dc) — country-routed via username. Geo targeting generates least-used OpenCode countries; every entry is validated through the gateway before import.",
  defaults: {
    enabled: false,
    apiKey: "",
    endpoint: DEFAULT_ENDPOINT,
    type: "res",
    country: "any",
    protocol: "http",
    perCountry: 3,
    validateImported: true,
    validationConcurrency: 10,
    minWorkingProxies: 1,
    intervalMinutes: 360,
    geoTargeting: "off",
    geoMaxCountries: 10,
  },
  configFields: [
    { name: "apiKey", type: "text", label: "API Key (pgw-...)" },
    { name: "endpoint", type: "text", label: "Gateway endpoint (host:port)" },
    { name: "type", type: "select", label: "Proxy type", options: [...VALID_TYPES] },
    { name: "protocol", type: "select", label: "Protocol", options: [...VALID_PROTOCOLS] },
    { name: "country", type: "text", label: "Country (ISO code or 'any')" },
    { name: "perCountry", type: "number", label: "Sticky sessions per country", min: 1, max: 50 },
    {
      name: "geoTargeting",
      type: "select",
      label: "Geo targeting",
      options: ["off", "opencode"],
      help: "'opencode' generates proxies for the least-used OpenCode countries (ascending token usage > threshold)",
    },
    { name: "geoMaxCountries", type: "number", label: "Max geo target countries", min: 1, max: 40 },
    { name: "validateImported", type: "boolean", label: "Validate before importing (always on for ProxyRise)" },
    { name: "validationConcurrency", type: "number", label: "Validation concurrency", min: 1, max: 50 },
    { name: "minWorkingProxies", type: "number", label: "Min working proxies", min: 0, max: 5000 },
    { name: "intervalMinutes", type: "number", label: "Refresh interval (minutes)", min: 30, max: 10080 },
  ],
  normalizeConfig(input = {}) {
    return {
      enabled: input.enabled === true,
      apiKey: typeof input.apiKey === "string" ? input.apiKey.trim() : "",
      endpoint: normalizeEndpoint(input.endpoint),
      type: VALID_TYPES.includes(input.type) ? input.type : "res",
      country: normalizeCountry(input.country),
      protocol: VALID_PROTOCOLS.includes(input.protocol) ? input.protocol : "http",
      perCountry: clampInt(input.perCountry, 3, 1, 50),
      // Test-first policy: never import unvalidated paid-bandwidth entries.
      validateImported: true,
      validationConcurrency: clampInt(input.validationConcurrency, 10, 1, 50),
      minWorkingProxies: clampInt(input.minWorkingProxies, 1, 0, 5000),
      intervalMinutes: clampInt(input.intervalMinutes, 360, 30, 10080),
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
    if (!normalized.apiKey) throw new Error("ProxyRise requires an API key (pgw-...)");

    const countries = [normalized.country];
    const proxies = [];

    for (const countryCode of countries) {
      for (let slot = 0; slot < normalized.perCountry; slot += 1) {
        const sid = stableSessionId(normalized.apiKey, normalized.type, countryCode, slot);
        const username =
          countryCode === "any"
            ? `${normalized.type}-sid-${sid}`
            : `${normalized.type}-${countryCode}-sid-${sid}`;
        const proxyUrl = `${normalized.protocol}://${encodeURIComponent(username)}:${encodeURIComponent(
          normalized.apiKey
        )}@${normalized.endpoint}`;
        proxies.push({
          proxyUrl,
          protocol: normalized.protocol,
          hostLabel: `${username}@${normalized.endpoint}`,
          username,
          country: countryCode.toUpperCase(),
        });
      }
    }

    return proxies;
  },
});
