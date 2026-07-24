import { DEFAULT_RETRY_CONFIG } from "../config/runtimeConfig.js";
import { PROVIDERS } from "../config/providers.js";
import {
  normalizeRetryPlugin,
  mergeRetryPlugins,
  normalizeUserRetryConfig,
  buildRuleList,
  matchErrorRule,
  decisionFromRule,
  matchHttpTextRetry,
  normalizeHttpRetry,
} from "./schema.js";

/** @type {Map<string, object>} */
const PLUGINS = new Map();

/**
 * Register a retry plugin (side-effect friendly).
 * Later register for same id replaces previous.
 * @param {string|object} idOrPlugin
 * @param {object} [maybePlugin]
 */
export function registerRetryPlugin(idOrPlugin, maybePlugin) {
  const raw = typeof idOrPlugin === "string"
    ? { ...(maybePlugin || {}), id: idOrPlugin }
    : idOrPlugin;
  const plugin = normalizeRetryPlugin(raw);
  if (!plugin) return null;
  PLUGINS.set(plugin.id, plugin);
  return plugin;
}

export function unregisterRetryPlugin(id) {
  return PLUGINS.delete(id);
}

export function clearRetryPlugins() {
  PLUGINS.clear();
}

export function listRetryPlugins() {
  return Array.from(PLUGINS.values()).map((p) => ({
    id: p.id,
    matchMode: p.matchMode,
    errorRules: p.errorRules,
    httpRetry: p.httpRetry,
    hasComputeRetryDelay: typeof p.computeRetryDelay === "function",
  }));
}

function pluginMatches(plugin, providerId) {
  if (!providerId) return false;
  if (plugin.id === "*") return true;
  if (plugin.matchMode === "exact") return plugin.id === providerId;
  if (plugin.matchMode === "prefix") {
    const prefix = plugin.id.endsWith("*") ? plugin.id.slice(0, -1) : plugin.id;
    return providerId === prefix || providerId.startsWith(prefix);
  }
  // glob-lite: trailing *
  if (plugin.id.endsWith("*")) {
    return providerId.startsWith(plugin.id.slice(0, -1));
  }
  return plugin.id === providerId;
}

/**
 * Collect built-in registry transport.retry as a synthetic plugin.
 */
function registryTransportPlugin(providerId) {
  const transport = PROVIDERS?.[providerId];
  if (!transport?.retry) return null;
  return normalizeRetryPlugin({
    id: providerId,
    httpRetry: transport.retry,
  });
}

/**
 * Resolve effective retry plugin for a provider + optional runtime overrides.
 * Merge order (later wins for httpRetry; errorRules earlier = higher priority):
 *   1. registered plugins matching provider (* → prefix → exact)
 *   2. registry transport.retry (when includeRegistry=true)
 *   3. credentials.providerSpecificData.retry (custom node)
 *   4. settingsOverride (providerStrategies[id].retry)
 *
 * Note: httpRetry on the result does NOT force DEFAULT_RETRY_CONFIG keys unless a
 * layer provided them — callers should merge defaults + executor config themselves.
 *
 * @param {string} providerId
 * @param {object|null} [credentials]
 * @param {object|null} [settingsOverride] - providerStrategies[providerId].retry
 * @param {{ includeRegistry?: boolean }} [opts]
 */
export function resolveRetryPlugin(providerId, credentials = null, settingsOverride = null, opts = {}) {
  const includeRegistry = opts.includeRegistry !== false;
  const matched = [];
  const exact = [];
  const prefix = [];
  const star = [];

  for (const plugin of PLUGINS.values()) {
    if (!pluginMatches(plugin, providerId)) continue;
    if (plugin.id === "*") star.push(plugin);
    else if (plugin.matchMode === "exact" || plugin.id === providerId) exact.push(plugin);
    else prefix.push(plugin);
  }

  // Lower priority first so later merge wins http keys; errorRules push order = priority
  matched.push(...star, ...prefix, ...exact);

  // Skip registry when executor already applied PROVIDERS[id].retry as this.config.retry
  // (includeRegistry=false) to avoid double-default overwriting delayMs:0 test configs.
  if (includeRegistry) {
    const reg = registryTransportPlugin(providerId);
    if (reg) matched.push(reg);
  }

  const nodeRetry = normalizeUserRetryConfig(credentials?.providerSpecificData?.retry);
  if (nodeRetry) {
    matched.push(normalizeRetryPlugin({ id: `${providerId}:node`, ...nodeRetry }));
  }

  const settingsRetry = normalizeUserRetryConfig(settingsOverride);
  if (settingsRetry) {
    matched.push(normalizeRetryPlugin({ id: `${providerId}:settings`, ...settingsRetry }));
  }

  if (matched.length === 0) {
    return {
      id: providerId || "_default",
      httpRetry: {},
      errorRules: [],
    };
  }

  return mergeRetryPlugins(...matched);
}

/**
 * HTTP status retry map for BaseExecutor / external callers (defaults filled in).
 */
export function resolveHttpRetryConfig(providerId, credentials = null, settingsOverride = null) {
  const plugin = resolveRetryPlugin(providerId, credentials, settingsOverride);
  return { ...DEFAULT_RETRY_CONFIG, ...normalizeHttpRetry(plugin.httpRetry) };
}

/**
 * Account/combo fallback classification with per-provider plugin rules.
 */
export function resolveFallbackDecision(status, errorText, backoffLevel = 0, providerId = null, credentials = null, settingsOverride = null) {
  const plugin = providerId
    ? resolveRetryPlugin(providerId, credentials, settingsOverride)
    : { errorRules: [] };
  const rules = buildRuleList(plugin);
  const matched = matchErrorRule(rules, status, errorText);
  return decisionFromRule(matched, backoffLevel);
}

/**
 * If error body/text should trigger same-account HTTP retry.
 */
export function resolveHttpTextRetry(providerId, status, errorText, credentials = null, settingsOverride = null) {
  const plugin = resolveRetryPlugin(providerId, credentials, settingsOverride);
  return matchHttpTextRetry(plugin, status, errorText);
}

export function getRetryComputeDelayHook(providerId, credentials = null, settingsOverride = null) {
  const plugin = resolveRetryPlugin(providerId, credentials, settingsOverride);
  return typeof plugin.computeRetryDelay === "function" ? plugin.computeRetryDelay : null;
}

// Re-exports for callers / tests
export {
  normalizeErrorText,
  errorTextIncludes,
  findMatchingErrorText,
  normalizeRetryPlugin,
  normalizeUserRetryConfig,
  normalizeErrorRule,
  normalizeHttpRetry,
  matchErrorRule,
  decisionFromRule,
  buildRuleList,
  matchHttpTextRetry,
  mergeRetryPlugins,
} from "./schema.js";

// Convenience alias — same as resolveFallbackDecision (accountFallback re-exports via service)
export { resolveFallbackDecision as checkFallbackError };
