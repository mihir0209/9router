import { BACKOFF_CONFIG } from "../config/errorConfig.js";
import { resolveFallbackDecision, resolveRetryPlugin } from "../retry/index.js";
import { buildRuleList, matchErrorRule } from "../retry/schema.js";

/**
 * Calculate exponential backoff cooldown for rate limits (429)
 * Level 1: 1s, Level 2: 2s, Level 3: 4s... → max 4 min
 * @param {number} backoffLevel - Current backoff level
 * @returns {number} Cooldown in milliseconds
 */
export function getQuotaCooldown(backoffLevel = 0) {
  const level = Math.max(0, backoffLevel - 1);
  const cooldown = BACKOFF_CONFIG.base * Math.pow(2, level);
  return Math.min(cooldown, BACKOFF_CONFIG.max);
}

/**
 * Check if error should trigger account fallback (switch to next account).
 * Always goes through the central retry matcher:
 *   lower = errorText.toLowerCase(); if (lower.includes(rule.text)) …
 * Plugin rules (per-provider) first, then global ERROR_RULES, then status.
 *
 * @param {number} status - HTTP status code
 * @param {string} errorText - Error message text
 * @param {number} backoffLevel - Current backoff level for exponential backoff
 * @param {string|null} [providerId] - Provider id for plugin resolution
 * @param {object|null} [credentials] - Credentials (node retry in providerSpecificData.retry)
 * @param {object|null} [settingsOverride] - providerStrategies[id].retry
 * @returns {{ shouldFallback: boolean, cooldownMs: number, newBackoffLevel?: number }}
 */
export function checkFallbackError(status, errorText, backoffLevel = 0, providerId = null, credentials = null, settingsOverride = null) {
  const decision = resolveFallbackDecision(status, errorText, backoffLevel, providerId, credentials, settingsOverride);
  // Upstream v0.5.81 fix(auth): don't cool down an account for a request-scoped 4xx
  // that matched no account rule — keeps healthy credentials in rotation for
  // context-overflow / validation errors. The central matcher returns a transient
  // fallback for any unmatched error; suppress it when no rule actually matched.
  if (decision.shouldFallback && decision.cooldownMs > 0) {
    const s = Number(status);
    const isRequestScoped4xx = s >= 400 && s < 500 && s !== 401 && s !== 402 && s !== 403 && s !== 429;
    if (isRequestScoped4xx) {
      // Text/status rules already won if they matched; only suppress the default.
      const plugin = providerId ? resolveRetryPlugin(providerId, credentials, settingsOverride) : { errorRules: [] };
      const rules = buildRuleList(plugin);
      if (!matchErrorRule(rules, status, errorText)) {
        return { shouldFallback: false, cooldownMs: 0 };
      }
    }
  }
  return decision;
}

/**
 * Check if account is currently unavailable (cooldown not expired)
 */
export function isAccountUnavailable(unavailableUntil) {
  if (!unavailableUntil) return false;
  return new Date(unavailableUntil).getTime() > Date.now();
}

/**
 * Calculate unavailable until timestamp
 */
export function getUnavailableUntil(cooldownMs) {
  return new Date(Date.now() + cooldownMs).toISOString();
}

/**
 * Get the earliest rateLimitedUntil from a list of accounts
 * @param {Array} accounts - Array of account objects with rateLimitedUntil
 * @returns {string|null} Earliest rateLimitedUntil ISO string, or null
 */
export function getEarliestRateLimitedUntil(accounts) {
  let earliest = null;
  const now = Date.now();
  for (const acc of accounts) {
    if (!acc.rateLimitedUntil) continue;
    const until = new Date(acc.rateLimitedUntil).getTime();
    if (until <= now) continue;
    if (!earliest || until < earliest) earliest = until;
  }
  if (!earliest) return null;
  return new Date(earliest).toISOString();
}

/**
 * Format rateLimitedUntil to human-readable "reset after Xm Ys"
 * @param {string} rateLimitedUntil - ISO timestamp
 * @returns {string} e.g. "reset after 2m 30s"
 */
export function formatRetryAfter(rateLimitedUntil) {
  if (!rateLimitedUntil) return "";
  const diffMs = new Date(rateLimitedUntil).getTime() - Date.now();
  if (diffMs <= 0) return "reset after 0s";
  const totalSec = Math.ceil(diffMs / 1000);
  const h = Math.floor(totalSec / 3600);
  const m = Math.floor((totalSec % 3600) / 60);
  const s = totalSec % 60;
  const parts = [];
  if (h > 0) parts.push(`${h}h`);
  if (m > 0) parts.push(`${m}m`);
  if (s > 0 || parts.length === 0) parts.push(`${s}s`);
  return `reset after ${parts.join(" ")}`;
}

/** Prefix for model lock flat fields on connection record */
export const MODEL_LOCK_PREFIX = "modelLock_";

/** Special key used when no model is known (account-level lock) */
export const MODEL_LOCK_ALL = `${MODEL_LOCK_PREFIX}__all`;

/** Build the flat field key for a model lock */
export function getModelLockKey(model) {
  return model ? `${MODEL_LOCK_PREFIX}${model}` : MODEL_LOCK_ALL;
}

/**
 * Check if a model lock on a connection is still active.
 * Reads flat field `modelLock_${model}` (or `modelLock___all` when model=null).
 */
export function isModelLockActive(connection, model) {
  const key = getModelLockKey(model);
  const expiry = connection[key] || connection[MODEL_LOCK_ALL];
  if (!expiry) return false;
  return new Date(expiry).getTime() > Date.now();
}

/**
 * Get earliest active model lock expiry across all modelLock_* fields.
 * Used for UI cooldown display.
 */
export function getEarliestModelLockUntil(connection) {
  if (!connection) return null;
  let earliest = null;
  const now = Date.now();
  for (const [key, val] of Object.entries(connection)) {
    if (!key.startsWith(MODEL_LOCK_PREFIX) || !val) continue;
    const t = new Date(val).getTime();
    if (t <= now) continue;
    if (!earliest || t < earliest) earliest = t;
  }
  return earliest ? new Date(earliest).toISOString() : null;
}

/**
 * Build update object to set a model lock on a connection.
 */
export function buildModelLockUpdate(model, cooldownMs) {
  const key = getModelLockKey(model);
  return { [key]: new Date(Date.now() + cooldownMs).toISOString() };
}

// ─── Extreme-write-mode in-memory lock registry ─────────────────────────────
// In extreme write mode, model locks are NOT persisted to providerConnections
// (zero DB writes). Instead they live in a process-global registry. Runtime
// notes: on Node this survives for the process lifetime; on Cloudflare Workers
// it lives per isolate — isolates are reused across requests but may be
// evicted or split across PoPs, so a lock loss (or per-colo divergence) only
// means the next request may retry a cooling account once. That failure mode
// is acceptable: the combo/account fallback absorbs a single wasted attempt.
// getProviderConnections() returns FRESH objects per read, so locks must live
// in this registry — mutating a connection object in memory is discarded.
if (!global._extremeModelLocks) global._extremeModelLocks = new Map(); // key: `${connectionId}|${model||__all}` → { untilMs, reason, status }

export function extremeModelLockKey(connectionId, model) {
  return `${connectionId}|${model || MODEL_LOCK_ALL}`;
}

export function setExtremeModelLock(connectionId, model, cooldownMs, meta = {}) {
  if (!connectionId) return;
  global._extremeModelLocks.set(extremeModelLockKey(connectionId, model), {
    untilMs: Date.now() + Math.max(0, cooldownMs),
    status: meta.status ?? null,
    reason: typeof meta.reason === "string" ? meta.reason.slice(0, 100) : null,
  });
}

export function clearExtremeModelLock(connectionId, model) {
  if (!connectionId) return;
  global._extremeModelLocks.delete(extremeModelLockKey(connectionId, model));
}

export function clearExpiredExtremeModelLocks(connectionId, now = Date.now()) {
  if (!connectionId) return;
  for (const [key, entry] of global._extremeModelLocks) {
    if (key.startsWith(`${connectionId}|`) && entry.untilMs <= now) {
      global._extremeModelLocks.delete(key);
    }
  }
}

export function isExtremeModelLockActive(connectionId, model, now = Date.now()) {
  if (!connectionId) return false;
  const entry = global._extremeModelLocks.get(extremeModelLockKey(connectionId, model));
  return !!entry && entry.untilMs > now;
}

export function getEarliestExtremeModelLockUntil(connectionId, now = Date.now()) {
  if (!connectionId) return null;
  let earliest = null;
  const prefix = `${connectionId}|`;
  for (const [key, entry] of global._extremeModelLocks) {
    if (!key.startsWith(prefix) || entry.untilMs <= now) continue;
    if (!earliest || entry.untilMs < earliest) earliest = entry.untilMs;
  }
  return earliest ? new Date(earliest).toISOString() : null;
}

export function getExtremeModelLockEntry(connectionId, model) {
  if (!connectionId) return null;
  return global._extremeModelLocks.get(extremeModelLockKey(connectionId, model)) || null;
}

/**
 * Build update object to clear all model locks on a connection.
 */
export function buildClearModelLocksUpdate(connection) {
  const cleared = {};
  for (const key of Object.keys(connection)) {
    if (key.startsWith(MODEL_LOCK_PREFIX)) cleared[key] = null;
  }
  return cleared;
}

/**
 * Filter available accounts (not in cooldown)
 */
export function filterAvailableAccounts(accounts, excludeId = null) {
  const now = Date.now();
  return accounts.filter(acc => {
    if (excludeId && acc.id === excludeId) return false;
    if (acc.rateLimitedUntil) {
      const until = new Date(acc.rateLimitedUntil).getTime();
      if (until > now) return false;
    }
    return true;
  });
}

/**
 * Reset account state when request succeeds
 * Clears cooldown and resets backoff level to 0
 * @param {object} account - Account object
 * @returns {object} Updated account with reset state
 */
export function resetAccountState(account) {
  if (!account) return account;
  return {
    ...account,
    rateLimitedUntil: null,
    backoffLevel: 0,
    lastError: null,
    status: "active"
  };
}

/**
 * Apply error state to account
 * @param {object} account - Account object
 * @param {number} status - HTTP status code
 * @param {string} errorText - Error message
 * @returns {object} Updated account with error state
 */
export function applyErrorState(account, status, errorText) {
  if (!account) return account;

  const backoffLevel = account.backoffLevel || 0;
  const providerId = account.provider || null;
  const credentials = account.providerSpecificData ? { providerSpecificData: account.providerSpecificData } : null;
  const { cooldownMs, newBackoffLevel } = checkFallbackError(status, errorText, backoffLevel, providerId, credentials);

  return {
    ...account,
    rateLimitedUntil: cooldownMs > 0 ? getUnavailableUntil(cooldownMs) : null,
    backoffLevel: newBackoffLevel ?? backoffLevel,
    lastError: { status, message: errorText, timestamp: new Date().toISOString() },
    status: "error"
  };
}
