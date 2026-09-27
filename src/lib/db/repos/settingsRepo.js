import { getAdapter } from "../driver.js";
import { parseJson, stringifyJson } from "../helpers/jsonCol.js";

const DEFAULT_MITM_ROUTER_BASE = `http://localhost:${process.env.PORT || "20128"}`;
const DEFAULT_HEADROOM_URL = process.env.HEADROOM_URL || "http://localhost:8787";

// "live"    — /v1/models asks every configured provider for its current models
//             (accurate, but one upstream round trip per provider per request).
// "cached"  — /v1/models answers from the database, refreshed explicitly via
//             the per-provider Refresh button or POST /api/models/refresh.
// Env var sets the default; the settings page can override it at runtime.
const VALID_MODELS_LIST_SOURCES = new Set(["live", "cached"]);
const ENV_MODELS_LIST_SOURCE = String(process.env.MODELS_LIST_SOURCE || "").trim().toLowerCase();
const DEFAULT_MODELS_LIST_SOURCE = VALID_MODELS_LIST_SOURCES.has(ENV_MODELS_LIST_SOURCE)
  ? ENV_MODELS_LIST_SOURCE
  : "live";

export function normalizeModelsListSource(value) {
  const v = String(value ?? "").trim().toLowerCase();
  return VALID_MODELS_LIST_SOURCES.has(v) ? v : DEFAULT_MODELS_LIST_SOURCE;
}

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
  gatewayControl: {
    disabled: false,
    reason: "",
    drain: false,
    blockedProviders: [],
    blockedModels: [],
  },
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
  // Where /v1/models gets its model list: "live" (query providers) or "cached"
  // (serve the DB copy). Default comes from MODELS_LIST_SOURCE; see above.
  modelsListSource: DEFAULT_MODELS_LIST_SOURCE,
};

const SETTINGS_CACHE_TTL_MS = 5 * 60 * 1000;
const CONTROL_DOMAIN = "settings";

if (!global._settingsRepoCache) {
  global._settingsRepoCache = { data: null, ts: 0, version: null };
}
const settingsRepoCache = global._settingsRepoCache;

export function resetSettingsCache() {
  settingsRepoCache.data = null;
  settingsRepoCache.ts = 0;
  settingsRepoCache.version = null;
}

/**
 * Drop the cached settings when another isolate has written since we last
 * looked. Without this a kill switch or provider block is invisible to other
 * Workers isolates for the full cache TTL.
 *
 * Clearing the repo cache is not enough on its own: the Cloudflare adapter
 * keeps its own in-memory copy of the row, refreshed from D1 only every few
 * minutes. A version change therefore has to force that refresh too, or the
 * next read just repopulates from the same stale copy.
 */
async function invalidateIfStale() {
  if (!settingsRepoCache.data) return;
  const { getControlVersion } = await import("@/lib/coordinator/controlVersion.js");
  const version = await getControlVersion(CONTROL_DOMAIN);
  if (version == null) return;
  if (settingsRepoCache.version == null) {
    settingsRepoCache.version = version;
    return;
  }
  if (version === settingsRepoCache.version) return;
  resetSettingsCache();
  try {
    const db = await getAdapter();
    if (typeof db.resync === "function") await db.resync(true, ["settings"]);
  } catch {}
}

async function readRaw() {
  const now = Date.now();
  if (settingsRepoCache.data && (now - settingsRepoCache.ts < SETTINGS_CACHE_TTL_MS)) {
    // Throttled to one Durable Object call per interval, so this is a memoized
    // read on the hot path and only occasionally a subrequest.
    await invalidateIfStale();
    if (settingsRepoCache.data && (Date.now() - settingsRepoCache.ts < SETTINGS_CACHE_TTL_MS)) {
      return { ...settingsRepoCache.data };
    }
  }
  const db = await getAdapter();
  // On Workers the adapter serves reads from its own in-memory copy, refreshed
  // from D1 only every few minutes. Reading a cold cache would capture that
  // stale copy and then stamp it with the *current* control version, after
  // which this isolate considers itself fresh and never corrects until the
  // adapter's own timer fires. Force the refresh so the data we cache always
  // matches the version we record alongside it.
  if (process.env.NINEROUTER_RUNTIME === "cloudflare" && typeof db.resync === "function") {
    await db.resync(true, ["settings"]).catch(() => {});
  }
  const row = db.get(`SELECT data FROM settings WHERE id = 1`);
  const data = row ? parseJson(row.data, {}) : {};
  settingsRepoCache.data = data;
  settingsRepoCache.ts = Date.now();
  const { getControlVersion } = await import("@/lib/coordinator/controlVersion.js");
  settingsRepoCache.version = await getControlVersion(CONTROL_DOMAIN);
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
  merged.modelsListSource = normalizeModelsListSource(merged.modelsListSource);
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
  // On Workers each isolate holds its own copy of the settings row, refreshed
  // from D1 only every few minutes. Merging against a stale copy would let an
  // isolate silently revert another isolate's change (e.g. re-enabling a key
  // someone just disabled, or undoing a kill switch). Writes are rare, so pay
  // for a forced refresh and merge against current state.
  if (typeof db.resync === "function") {
    await db.resync(true, ["settings"]).catch(() => {});
  }
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
  const { bumpControlVersion } = await import("@/lib/coordinator/controlVersion.js");
  await bumpControlVersion(CONTROL_DOMAIN);
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
