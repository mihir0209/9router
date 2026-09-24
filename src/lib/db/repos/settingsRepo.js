import { getAdapter } from "../driver.js";
import { parseJson, stringifyJson } from "../helpers/jsonCol.js";

const DEFAULT_MITM_ROUTER_BASE = `http://localhost:${process.env.PORT || "20128"}`;
const DEFAULT_HEADROOM_URL = process.env.HEADROOM_URL || "http://localhost:8787";

const DEFAULT_SETTINGS = {
  cloudEnabled: false,
  tunnelEnabled: false,
  tunnelUrl: "",
  tunnelProvider: "cloudflare",
  tailscaleEnabled: false,
  tailscaleUrl: "",
  stickyRoundRobinLimit: 3,
  providerStrategies: {},
  quotaVisibility: {},
  comboStrategy: "fallback",
  comboStickyRoundRobinLimit: 1,
  comboStrategies: {},
  capacityAdapter: {
    vision: { enabled: true, roundRobin: false, models: [] },
    pdf: { enabled: false, roundRobin: false, models: [] },
    audioInput: { enabled: true, roundRobin: false, models: [] },
    videoInput: { enabled: false, roundRobin: false, models: [] },
  },
  requireLogin: true,
  requireApiKey: true,
  tunnelDashboardAccess: true,
  authMode: "password",
  ssoType: "oidc",
  oidcIssuerUrl: "",
  oidcClientId: "",
  oidcClientSecret: "",
  oidcScopes: "openid profile email",
  oidcLoginLabel: "Sign in with OIDC",
  samlEntryPoint: "",
  samlIssuer: "urn:9router:sp",
  samlCert: "",
  samlLoginLabel: "Sign in with SAML SSO",
  samlAttributeEmail: "email",
  samlAttributeName: "name",
  enableObservability: false,
  observabilityMaxRecords: 1000,
  observabilityBatchSize: 20,
  observabilityFlushIntervalMs: 5000,
  observabilityMaxJsonSize: 5,
  // Tri-state: null = Auto (on for Cloudflare, off locally), true = always
  // on, false = always off. Skips per-request log writes (usageHistory rows,
  // request details) while keeping daily aggregates for analytics.
  lowWriteMode: null,
  analyticsBufferMinutes: 10,
  analyticsBufferEnabled: null,
  // Structured output (response_format / json_schema) handling.
  // Values: "auto" (default) | "passthrough" | "fallback".
  //   "auto" — preserve/translate natively for doc-verified base providers
  //     (openai, azure, groq, together, fireworks, xai, mistral, openrouter,
  //     vertex-partner; Gemini/Ollama via their native equivalents),
  //     prompt-injection fallback otherwise (Claude-family, Kiro,
  //     json_object-only bases like deepseek, undocumented bases, and custom
  //     openai-compatible-* gateway nodes).
  //   "passthrough" — never downgrade; forward verbatim (OpenAI targets) or
  //     via native equivalents (Gemini/Ollama/Responses). Upstreams that
  //     reject it 400 are skipped via combo fallback.
  //   "fallback" — legacy: always downgrade json_schema to json_object +
  //     system-prompt injection. Maximum compatibility, no enforcement.
  // Per-provider overrides: { [providerOrNodeId]: "auto"|"passthrough"|"fallback" }
  structuredOutputMode: "auto",
  structuredOutputOverrides: {},
  // Extreme low-write mode: lowWriteMode === "extreme" skips ALL analytics DB
  // writes (usageDaily aggregates + lifetime counters included — dashboard
  // usage and budget enforcement degrade to in-memory). For metered stores
  // (Cloudflare D1 100k writes/day). Plain true/false/null keep prior behavior.
  // See usageRepo.isExtremeWriteMode().
  outboundProxyEnabled: false,
  outboundProxyUrl: "",
  outboundNoProxy: "",
  mitmRouterBaseUrl: DEFAULT_MITM_ROUTER_BASE,
  dnsToolEnabled: {},
  rtkEnabled: true,
  filterClaudeIdentityHeaders: false,
  stripRequestUser: false,
  headroomEnabled: false,
  headroomUrl: DEFAULT_HEADROOM_URL,
  headroomCompressUserMessages: false,
  headroomTimeoutMs: 3000,
  cavemanEnabled: false,
  cavemanLevel: "full",
  ponytailEnabled: false,
  ponytailLevel: "full",
  pxpipeEnabled: false,
  pxpipeAutoInstall: true,
  pxpipeMinChars: 25000,
  pxpipeTimeoutMs: 15000,
  autoBlockedIPs: [],
  proxyscrapeProxyPool: {
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
  },
  proxyPoolProviders: {},
  proxyPoolQuarantine: { global: {}, scoped: {} },
  proxyPoolRateLimitCooldownMinutes: 30,
  proxyPoolNetworkCooldownMinutes: 10,
};

const SETTINGS_CACHE_TTL_MS = 5 * 60 * 1000;

if (!global._settingsRepoCache) {
  global._settingsRepoCache = { data: null, ts: 0 };
}
const settingsRepoCache = global._settingsRepoCache;

export function resetSettingsCache() {
  settingsRepoCache.data = null;
  settingsRepoCache.ts = 0;
}

async function readRaw() {
  const now = Date.now();
  if (settingsRepoCache.data && (now - settingsRepoCache.ts < SETTINGS_CACHE_TTL_MS)) {
    return { ...settingsRepoCache.data };
  }
  const db = await getAdapter();
  if (typeof db.resync === "function") {
    await db.resync().catch(() => {});
  }
  const row = db.get(`SELECT data FROM settings WHERE id = 1`);
  const data = row ? parseJson(row.data, {}) : {};
  settingsRepoCache.data = data;
  settingsRepoCache.ts = now;
  return { ...data };
}

// Merge raw settings with defaults; backward-compat for missing keys
export function mergeWithDefaults(raw) {
  const merged = { ...DEFAULT_SETTINGS, ...(raw || {}) };
  for (const [key, defVal] of Object.entries(DEFAULT_SETTINGS)) {
    if (merged[key] === undefined) {
      if (
        key === "outboundProxyEnabled" &&
        typeof merged.outboundProxyUrl === "string" &&
        merged.outboundProxyUrl.trim()
      ) {
        merged[key] = true;
      } else {
        merged[key] = defVal;
      }
    }
  }
  if (merged.capacityAdapter && typeof merged.capacityAdapter === "object") {
    for (const capKey of Object.keys(merged.capacityAdapter)) {
      const entry = merged.capacityAdapter[capKey];
      if (Array.isArray(entry?.models)) {
        entry.models = entry.models.map((m) =>
          m === "oc/mimo-v2.5-free" ? "oc/mimo-v2.6-flash-free" : m
        );
      }
    }
  }
  return merged;
}

export async function getSettings() {
  const raw = await readRaw();
  return mergeWithDefaults(raw);
}

// Atomic read-merge-write inside transaction (prevents losing concurrent updates)
export async function updateSettings(updates) {
  const db = await getAdapter();
  let next;
  db.transaction(function () {
    const row = db.get(`SELECT data FROM settings WHERE id = 1`);
    const current = row ? parseJson(row.data, {}) : {};
    next = { ...current, ...updates };
    db.run(
      `INSERT INTO settings(id, data) VALUES(1, ?) ON CONFLICT(id) DO UPDATE SET data = excluded.data`,
      [stringifyJson(next)],
    );
  });
  resetSettingsCache();
  return mergeWithDefaults(next);
}

export async function isCloudEnabled() {
  const settings = await getSettings();
  return settings.cloudEnabled === true;
}

export async function getCloudUrl() {
  const settings = await getSettings();
  return (
    settings.cloudUrl ||
    process.env.CLOUD_URL ||
    process.env.NEXT_PUBLIC_CLOUD_URL ||
    ""
  );
}

export async function exportSettings() {
  return await readRaw();
}
