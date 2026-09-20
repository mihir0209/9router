import { getSettings, updateSettings } from "@/lib/localDb";

const DEFAULT_RATE_LIMIT_COOLDOWN_MINUTES = 30;
const DEFAULT_NETWORK_COOLDOWN_MINUTES = 10;

let inMemoryQuarantine = null;

function normalizeString(value) {
  return value === undefined || value === null ? "" : String(value).trim();
}

function keyPart(value) {
  return normalizeString(value) || "*";
}

export function buildProxyScopeKey(provider, model) {
  return `${keyPart(provider)}:${keyPart(model)}`;
}

function normalizeStore(store = {}) {
  return {
    global: store.global && typeof store.global === "object" ? store.global : {},
    scoped: store.scoped && typeof store.scoped === "object" ? store.scoped : {},
  };
}

function activeEntry(entry, now = Date.now()) {
  if (!entry?.until) return null;
  const untilMs = new Date(entry.until).getTime();
  if (!Number.isFinite(untilMs) || untilMs <= now) return null;
  return entry;
}

export function isProxyQuarantined(store, proxyPoolId, { provider, model } = {}, now = Date.now()) {
  const id = normalizeString(proxyPoolId);
  if (!id) return false;
  const normalized = normalizeStore(store);
  if (activeEntry(normalized.global[id], now)) return true;

  const scopeKey = buildProxyScopeKey(provider, model);
  return !!activeEntry(normalized.scoped[scopeKey]?.[id], now);
}

function pruneStore(store, now = Date.now()) {
  const normalized = normalizeStore(store);
  const next = { global: {}, scoped: {} };

  for (const [proxyPoolId, entry] of Object.entries(normalized.global)) {
    if (activeEntry(entry, now)) next.global[proxyPoolId] = entry;
  }

  for (const [scopeKey, entries] of Object.entries(normalized.scoped)) {
    const scopedEntries = {};
    for (const [proxyPoolId, entry] of Object.entries(entries || {})) {
      if (activeEntry(entry, now)) scopedEntries[proxyPoolId] = entry;
    }
    if (Object.keys(scopedEntries).length > 0) next.scoped[scopeKey] = scopedEntries;
  }

  return next;
}

function addMinutes(minutes) {
  return new Date(Date.now() + Math.max(1, minutes) * 60_000).toISOString();
}

export async function getProxyPoolQuarantine() {
  const { isExtremeWriteMode } = await import("@/lib/db/repos/usageRepo.js");
  if (await isExtremeWriteMode().catch(() => false)) {
    if (!inMemoryQuarantine) {
      const settings = await getSettings();
      inMemoryQuarantine = pruneStore(settings.proxyPoolQuarantine);
    }
    return pruneStore(inMemoryQuarantine);
  }
  const settings = await getSettings();
  return pruneStore(settings.proxyPoolQuarantine);
}

export async function quarantineProxyPool(proxyPoolId, { provider, model, reason, kind } = {}) {
  const id = normalizeString(proxyPoolId);
  if (!id) return null;

  const { isExtremeWriteMode } = await import("@/lib/db/repos/usageRepo.js");
  const extreme = await isExtremeWriteMode().catch(() => false);

  const settings = await getSettings();
  const store = pruneStore(extreme && inMemoryQuarantine ? inMemoryQuarantine : settings.proxyPoolQuarantine);
  const reasonText = normalizeString(reason).slice(0, 300) || "Proxy quarantined";

  if (kind === "global") {
    const minutes = Number(settings.proxyPoolNetworkCooldownMinutes) || DEFAULT_NETWORK_COOLDOWN_MINUTES;
    store.global[id] = {
      until: addMinutes(minutes),
      reason: reasonText,
      count: (store.global[id]?.count || 0) + 1,
    };
  } else {
    const scopeKey = buildProxyScopeKey(provider, model);
    const minutes = Number(settings.proxyPoolRateLimitCooldownMinutes) || DEFAULT_RATE_LIMIT_COOLDOWN_MINUTES;
    store.scoped[scopeKey] ||= {};
    store.scoped[scopeKey][id] = {
      until: addMinutes(minutes),
      reason: reasonText,
      count: (store.scoped[scopeKey][id]?.count || 0) + 1,
    };
  }

  if (extreme) {
    inMemoryQuarantine = store;
    return store;
  }

  await updateSettings({ proxyPoolQuarantine: store });
  return store;
}

export function classifyProxyFailure(status, errorText) {
  const text = normalizeString(errorText).toLowerCase();
  if (status === 429 || text.includes("freeusagelimiterror") || text.includes("rate limit exceeded")) {
    return "scoped";
  }

  if (
    text.includes("proxy") ||
    text.includes("connect timeout") ||
    text.includes("und_err") ||
    text.includes("econn") ||
    text.includes("socket") ||
    text.includes("certificate") ||
    text.includes("cert_has_expired") ||
    text.includes("tls")
  ) {
    return "global";
  }

  return null;
}
