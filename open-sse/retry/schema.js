import { resolveRetryEntry } from "../config/runtimeConfig.js";
import { BACKOFF_CONFIG, ERROR_RULES, TRANSIENT_COOLDOWN_MS } from "../config/errorConfig.js";

export const RETRY_ACTIONS = {
  FALLBACK: "fallback",
  HTTP_RETRY: "httpRetry",
  NONE: "none",
};

/**
 * Central plain-text error normalizer.
 * Always: stringify non-strings → toLowerCase(). Empty/null → "".
 *
 *   const lower = normalizeErrorText(errorBody);
 *   if (lower.includes("insufficient tokens")) { ... }
 *
 * @param {unknown} errorText
 * @returns {string} lowercased error text
 */
export function normalizeErrorText(errorText) {
  if (errorText == null || errorText === "") return "";
  if (typeof errorText === "string") return errorText.toLowerCase();
  try {
    return JSON.stringify(errorText).toLowerCase();
  } catch {
    return String(errorText).toLowerCase();
  }
}

/**
 * THE plain-text match: lower.includes(needle).
 * Needle is lowercased/trimmed so callers can pass UI/raw substrings.
 *
 *   if (errorTextIncludes(body, "rate limit")) { ... }
 *
 * @param {unknown} errorText - raw upstream body/message
 * @param {string} needle - substring to look for (case-insensitive)
 * @returns {boolean}
 */
export function errorTextIncludes(errorText, needle) {
  if (typeof needle !== "string" || !needle.trim()) return false;
  const lower = normalizeErrorText(errorText);
  if (!lower) return false;
  return lower.includes(needle.trim().toLowerCase());
}

/**
 * First matching substring from a list (order = priority).
 * @param {unknown} errorText
 * @param {string[]} needles
 * @returns {string|null} the matched needle (original list entry), or null
 */
export function findMatchingErrorText(errorText, needles) {
  if (!Array.isArray(needles) || needles.length === 0) return null;
  const lower = normalizeErrorText(errorText);
  if (!lower) return null;
  for (const needle of needles) {
    if (typeof needle !== "string" || !needle.trim()) continue;
    if (lower.includes(needle.trim().toLowerCase())) return needle;
  }
  return null;
}

/**
 * Normalize one error classification rule.
 * @param {object} rule
 * @returns {object|null}
 */
export function normalizeErrorRule(rule) {
  if (!rule || typeof rule !== "object") return null;
  const out = {};
  if (typeof rule.text === "string" && rule.text.trim()) {
    out.text = rule.text.trim().toLowerCase();
  }
  if (rule.status != null) {
    const status = Number(rule.status);
    if (!Number.isFinite(status)) return null;
    out.status = status;
  }
  if (!out.text && out.status == null) return null;

  if (rule.backoff === true) out.backoff = true;
  if (rule.cooldownMs != null) {
    const ms = Number(rule.cooldownMs);
    if (Number.isFinite(ms) && ms >= 0) out.cooldownMs = ms;
  }
  if (rule.shouldFallback === false) out.shouldFallback = false;
  else out.shouldFallback = true;

  // HTTP same-account retry triggered by body/text (or status) match
  if (rule.retryHttp === true || rule.httpRetry === true) {
    out.retryHttp = true;
    const attempts = Number(rule.attempts);
    out.attempts = Number.isFinite(attempts) && attempts > 0 ? attempts : 1;
    const delayMs = Number(rule.delayMs);
    out.delayMs = Number.isFinite(delayMs) && delayMs >= 0 ? delayMs : 2000;
  }

  return out;
}

/**
 * Normalize HTTP status→retry map.
 * @param {object|null|undefined} httpRetry
 * @returns {Record<string, {attempts:number, delayMs:number}>}
 */
export function normalizeHttpRetry(httpRetry) {
  if (!httpRetry || typeof httpRetry !== "object") return {};
  const out = {};
  for (const [key, value] of Object.entries(httpRetry)) {
    const statusKey = String(key);
    const entry = resolveRetryEntry(value);
    if (entry.attempts > 0) out[statusKey] = entry;
    else if (value === 0 || value?.attempts === 0) out[statusKey] = { attempts: 0, delayMs: entry.delayMs };
  }
  return out;
}

/**
 * Normalize a full plugin definition.
 * @param {object} plugin
 * @returns {object|null}
 */
export function normalizeRetryPlugin(plugin) {
  if (!plugin || typeof plugin !== "object") return null;
  const id = typeof plugin.id === "string" && plugin.id.trim() ? plugin.id.trim() : null;
  if (!id) return null;

  const errorRules = Array.isArray(plugin.errorRules)
    ? plugin.errorRules.map(normalizeErrorRule).filter(Boolean)
    : [];

  const httpRetry = normalizeHttpRetry(plugin.httpRetry || plugin.retry);

  const out = {
    id,
    errorRules,
    httpRetry,
  };

  if (typeof plugin.computeRetryDelay === "function") {
    out.computeRetryDelay = plugin.computeRetryDelay;
  }

  if (plugin.matchMode === "prefix" || plugin.matchMode === "exact" || plugin.matchMode === "glob") {
    out.matchMode = plugin.matchMode;
  } else if (id.endsWith("*") || id.includes("/")) {
    out.matchMode = "prefix";
  } else {
    out.matchMode = "exact";
  }

  return out;
}

/**
 * Merge multiple plugins (later wins for httpRetry keys; errorRules earlier = higher priority).
 * Does not inject DEFAULT_RETRY_CONFIG — callers merge defaults at the edge.
 * @param {...object} plugins
 */
export function mergeRetryPlugins(...plugins) {
  const list = plugins.filter(Boolean);
  if (list.length === 0) {
    return {
      id: "_merged",
      errorRules: [],
      httpRetry: {},
    };
  }

  const httpRetry = {};
  const errorRules = [];
  let computeRetryDelay = null;
  let id = list[list.length - 1].id || "_merged";

  for (const p of list) {
    if (p.httpRetry) Object.assign(httpRetry, normalizeHttpRetry(p.httpRetry));
    if (Array.isArray(p.errorRules) && p.errorRules.length) {
      errorRules.push(...p.errorRules.map(normalizeErrorRule).filter(Boolean));
    }
    if (typeof p.computeRetryDelay === "function") computeRetryDelay = p.computeRetryDelay;
    if (p.id) id = p.id;
  }

  const merged = { id, httpRetry, errorRules };
  if (computeRetryDelay) merged.computeRetryDelay = computeRetryDelay;
  return merged;
}

/**
 * Match rules against status + error text.
 * Text first via errorTextIncludes (lower.includes), then status.
 * @returns {object|null} matched rule
 */
export function matchErrorRule(rules, status, errorText) {
  if (!Array.isArray(rules) || rules.length === 0) return null;

  // Pass 1: plain-text lower.includes(rule.text) — order = priority
  for (const rule of rules) {
    if (rule.text && errorTextIncludes(errorText, rule.text)) return rule;
  }
  // Pass 2: HTTP status
  for (const rule of rules) {
    if (rule.status != null && rule.status === status) return rule;
  }
  return null;
}

/**
 * Classify fallback decision from matched rule + backoff level.
 */
export function decisionFromRule(rule, backoffLevel = 0) {
  if (!rule) {
    return { shouldFallback: true, cooldownMs: TRANSIENT_COOLDOWN_MS };
  }
  if (rule.shouldFallback === false) {
    return { shouldFallback: false, cooldownMs: 0 };
  }
  if (rule.backoff) {
    const newLevel = Math.min((backoffLevel || 0) + 1, BACKOFF_CONFIG.maxLevel);
    const level = Math.max(0, newLevel - 1);
    const cooldown = Math.min(BACKOFF_CONFIG.base * Math.pow(2, level), BACKOFF_CONFIG.max);
    return { shouldFallback: true, cooldownMs: cooldown, newBackoffLevel: newLevel };
  }
  return {
    shouldFallback: true,
    cooldownMs: rule.cooldownMs != null ? rule.cooldownMs : TRANSIENT_COOLDOWN_MS,
  };
}

/**
 * Build combined rule list: plugin rules first, then global ERROR_RULES.
 */
export function buildRuleList(plugin) {
  const pluginRules = Array.isArray(plugin?.errorRules) ? plugin.errorRules : [];
  return [...pluginRules, ...ERROR_RULES.map(normalizeErrorRule).filter(Boolean)];
}

/**
 * Find a text/status rule that requests same-account HTTP retry.
 * Checks plugin rules first (higher priority), then global ERROR_RULES.
 */
export function matchHttpTextRetry(plugin, status, errorText) {
  const pluginRules = Array.isArray(plugin?.errorRules) ? plugin.errorRules : [];
  const globalRules = ERROR_RULES.map(normalizeErrorRule).filter(Boolean);
  const allRules = [...pluginRules, ...globalRules];
  const matched = matchErrorRule(allRules.filter((r) => r.retryHttp), status, errorText);
  if (!matched) return null;
  return {
    attempts: matched.attempts || 1,
    delayMs: matched.delayMs != null ? matched.delayMs : 2000,
    rule: matched,
  };
}

/**
 * Normalize user/UI config blob (node.retry or providerStrategies[id].retry).
 */
export function normalizeUserRetryConfig(raw) {
  if (!raw || typeof raw !== "object") return null;
  const plugin = normalizeRetryPlugin({
    id: raw.id || "_user",
    httpRetry: raw.httpRetry || raw.retry,
    errorRules: raw.errorRules || raw.rules,
  });
  if (!plugin) return null;
  if (!plugin.errorRules.length && Object.keys(plugin.httpRetry).length === 0) return null;
  return {
    httpRetry: plugin.httpRetry,
    errorRules: plugin.errorRules,
  };
}
