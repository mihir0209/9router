import { HTTP_STATUS, DEFAULT_RETRY_CONFIG, resolveRetryEntry, FETCH_CONNECT_TIMEOUT_MS } from "../config/runtimeConfig.js";
import { shouldRefreshCredentials } from "../services/oauthCredentialManager.js";
import { proxyAwareFetch } from "../utils/proxyFetch.js";
import { dbg } from "../utils/debugLog.js";
import { ANTHROPIC_API_VERSION, OPENAI_COMPAT_BASE, ANTHROPIC_COMPAT_BASE } from "../providers/shared.js";
import {
  resolveRetryPlugin,
  resolveHttpTextRetry,
  getRetryComputeDelayHook,
} from "../retry/index.js";
import { resolveHeaderTemplates, applyHeaderBlacklist } from "../utils/headerTemplates.js";
import { resolveOpenAICompatibleApiType } from "../services/provider.js";

/**
 * BaseExecutor - Base class for provider executors
 */
export class BaseExecutor {
  constructor(provider, config) {
    this.provider = provider;
    this.config = config;
    this.noAuth = config?.noAuth || false;
  }

  getProvider() {
    return this.provider;
  }

  getBaseUrls() {
    return this.config.baseUrls || (this.config.baseUrl ? [this.config.baseUrl] : []);
  }

  getFallbackCount() {
    return this.getBaseUrls().length || 1;
  }

  buildUrl(model, stream, urlIndex = 0, credentials = null) {
    if (this.provider?.startsWith?.("openai-compatible-")) {
      const baseUrl = credentials?.providerSpecificData?.baseUrl || OPENAI_COMPAT_BASE;
      const normalized = baseUrl.replace(/\/$/, "");
      if (normalized.endsWith("/chat/completions") || normalized.endsWith("/responses") || normalized.includes("/chat-completions")) {
        return normalized;
      }
      const path = resolveOpenAICompatibleApiType(this.provider, credentials) === "responses" ? "/responses" : "/chat/completions";
      return `${normalized}${path}`;
    }
    if (this.provider?.startsWith?.("anthropic-compatible-")) {
      const baseUrl = credentials?.providerSpecificData?.baseUrl || ANTHROPIC_COMPAT_BASE;
      const normalized = baseUrl.replace(/\/$/, "");
      if (normalized.endsWith("/messages")) {
        return normalized;
      }
      return `${normalized}/messages`;
    }
    const baseUrls = this.getBaseUrls();
    return baseUrls[urlIndex] || baseUrls[0] || this.config.baseUrl;
  }

  buildHeaders(credentials, stream = true) {
    const headers = {
      "Content-Type": "application/json",
      ...this.config.headers
    };

    // Apply custom headers from provider node configuration (before auth)
    const customHeaders = credentials?.providerSpecificData?.customHeaders;
    if (customHeaders && typeof customHeaders === "object") {
      const resolved = resolveHeaderTemplates(customHeaders);
      Object.assign(headers, resolved);
    }

    if (this.provider?.startsWith?.("anthropic-compatible-")) {
      // Anthropic-compatible providers use x-api-key header
      if (credentials.apiKey) {
        headers["x-api-key"] = credentials.apiKey;
      } else if (credentials.accessToken) {
        headers["Authorization"] = `Bearer ${credentials.accessToken}`;
      }
      if (!headers["anthropic-version"]) {
        headers["anthropic-version"] = ANTHROPIC_API_VERSION;
      }
    } else {
      // Standard Bearer token auth for other providers
      if (credentials.accessToken) {
        headers["Authorization"] = `Bearer ${credentials.accessToken}`;
      } else if (credentials.apiKey) {
        headers["Authorization"] = `Bearer ${credentials.apiKey}`;
      }
    }

    if (stream) {
      headers["Accept"] = "text/event-stream";
    }

    // Apply header blacklist (after all headers are set)
    const blacklist = credentials?.providerSpecificData?.headerBlacklist;
    if (blacklist && Array.isArray(blacklist) && blacklist.length > 0) {
      return applyHeaderBlacklist(headers, blacklist);
    }

    return headers;
  }

  // Override in subclass for provider-specific transformations
  transformRequest(model, body, stream, credentials) {
    return body;
  }

  shouldRetry(status, urlIndex) {
    return status === HTTP_STATUS.RATE_LIMITED && urlIndex + 1 < this.getFallbackCount();
  }

  // Override in subclass for provider-specific refresh
  async refreshCredentials(credentials, log, proxyOptions = null) {
    return null;
  }

  needsRefresh(credentials) {
    return shouldRefreshCredentials(this.provider, credentials);
  }

  parseError(response, bodyText) {
    return { status: response.status, message: bodyText || `HTTP ${response.status}` };
  }

  isClientRetryableText(errorText) {
    const text = String(errorText || "").toLowerCase();
    return text.includes("temporarily unavailable") || text.includes("service unavailable");
  }

  retryableClientHeaders(headers) {
    const next = new Headers();
    if (headers?.forEach) {
      headers.forEach((value, key) => next.set(key, value));
    } else {
      const contentType = headers?.get?.("content-type");
      if (contentType) next.set("content-type", contentType);
    }
    return next;
  }

  async execute({ model, body, stream, credentials, signal, log, proxyOptions = null }) {
    const fallbackCount = this.getFallbackCount();
    let lastError = null;
    let lastStatus = 0;
    const retryAttemptsByUrl = {};

    // Merge: defaults → executor/registry config → plugin/node/settings overlays
    // includeRegistry:false — this.config.retry already is PROVIDERS[id].retry
    const plugin = resolveRetryPlugin(this.provider, credentials, null, { includeRegistry: false });
    const retryConfig = {
      ...DEFAULT_RETRY_CONFIG,
      ...this.config.retry,
      ...plugin.httpRetry,
    };
    const pluginDelayHook = typeof plugin.computeRetryDelay === "function"
      ? plugin.computeRetryDelay
      : getRetryComputeDelayHook(this.provider, credentials);

    // Schedule retry via retryConfig[statusKey]. Returns true when caller should `urlIndex--; continue`
    // response (optional) lets a subclass/plugin hook compute a dynamic delay (e.g. antigravity Retry-After).
    const tryRetry = async (urlIndex, statusKey, reason, response = null, overrideEntry = null) => {
      const { attempts, delayMs } = resolveRetryEntry(overrideEntry || retryConfig[statusKey]);
      if (attempts <= 0 || retryAttemptsByUrl[urlIndex] >= attempts) return false;
      // Hook: subclass may derive delay from the response (headers/body). null → skip retry, use fallback.
      let waitMs = delayMs;
      const delayHook = this.computeRetryDelay || pluginDelayHook;
      if (response && delayHook) {
        const dynamic = await delayHook.call(this, response, retryAttemptsByUrl[urlIndex] + 1, delayMs);
        if (dynamic === false) return false; // hook vetoes retry (e.g. Retry-After too long)
        if (dynamic != null) waitMs = dynamic;
      }
      retryAttemptsByUrl[urlIndex]++;
      log?.debug?.("RETRY", `${reason} retry ${retryAttemptsByUrl[urlIndex]}/${attempts} after ${waitMs / 1000}s`);
      await new Promise(resolve => setTimeout(resolve, waitMs));
      return true;
    };

    for (let urlIndex = 0; urlIndex < fallbackCount; urlIndex++) {
      const url = this.buildUrl(model, stream, urlIndex, credentials);
      const transformedBody = this.transformRequest(model, body, stream, credentials);
      const headers = this.buildHeaders(credentials, stream, url, model);

      if (!retryAttemptsByUrl[urlIndex]) retryAttemptsByUrl[urlIndex] = 0;

      // Abort if upstream doesn't return response headers within connection timeout
      const connectCtrl = new AbortController();
      const timeoutMs = this.config?.timeoutMs || FETCH_CONNECT_TIMEOUT_MS;
      const connectTimer = setTimeout(() => connectCtrl.abort(new Error("fetch connect timeout")), timeoutMs);
      const mergedSignal = signal ? AbortSignal.any([signal, connectCtrl.signal]) : connectCtrl.signal;

      try {
        const bodyStr = JSON.stringify(transformedBody);
        // DEBUG: dump upstream request when KIRO_DEBUG is set
        if (process.env.KIRO_DEBUG && this.provider === "kiro") {
          try {
            const { writeFileSync } = await import("node:fs");
            const dump = JSON.stringify({ provider: this.provider, url, headers, body: transformedBody }, null, 2);
            writeFileSync("/tmp/kiro-debug-upstream-request.json", dump);
            console.error("[KIRO_DEBUG] upstream request → /tmp/kiro-debug-upstream-request.json");
          } catch (e) { console.error(`[KIRO_DEBUG] dump failed: ${e.message}`); }
        }
        const fetchT0 = Date.now();
        dbg("FETCH", `${this.provider.toUpperCase()} → ${url} | body=${bodyStr.length}B | connectTimeout=${timeoutMs}ms`);
        let response = await proxyAwareFetch(url, {
          method: "POST",
          headers,
          body: bodyStr,
          signal: mergedSignal
        }, proxyOptions);
        clearTimeout(connectTimer);
        const ct = response.headers?.get?.("content-type") || "";
        const cl = response.headers?.get?.("content-length") || "?";
        dbg("FETCH", `${this.provider.toUpperCase()} ← ${response.status} | ttft=${Date.now() - fetchT0}ms | ct=${ct} | cl=${cl}`);

        if (await tryRetry(urlIndex, response.status, `status ${response.status}`, response)) { urlIndex--; continue; }

        // Plugin text rules: plain-text match on error body → same-account HTTP retry
        if (response.status >= 400) {
          let errBody = "";
          try { errBody = await response.clone().text(); } catch { /* ignore */ }
          // DEBUG: dump upstream error response when KIRO_DEBUG is set
          if (process.env.KIRO_DEBUG && this.provider === "kiro") {
            try {
              const { writeFileSync } = await import("node:fs");
              const dump = JSON.stringify({ status: response.status, headers: Object.fromEntries(response.headers?.entries?.() || []), body: errBody }, null, 2);
              writeFileSync("/tmp/kiro-debug-upstream-response.json", dump);
              console.error("[KIRO_DEBUG] upstream response → /tmp/kiro-debug-upstream-response.json");
            } catch (e) { console.error(`[KIRO_DEBUG] response dump failed: ${e.message}`); }
          }
          const textRetry = resolveHttpTextRetry(this.provider, response.status, errBody, credentials);
          if (textRetry && await tryRetry(urlIndex, response.status, `text "${textRetry.rule?.text || "match"}"`, response, textRetry)) {
            urlIndex--;
            continue;
          }
          if (textRetry && this.isClientRetryableText(errBody) && response.status !== HTTP_STATUS.RATE_LIMITED) {
            response = new Response(errBody, {
              status: HTTP_STATUS.RATE_LIMITED,
              statusText: "Too Many Requests",
              headers: this.retryableClientHeaders(response.headers),
            });
          }
        }

        if (this.shouldRetry(response.status, urlIndex)) {
          log?.debug?.("RETRY", `${response.status} on ${url}, trying fallback ${urlIndex + 1}`);
          lastStatus = response.status;
          continue;
        }

        return { response, url, headers, transformedBody };
      } catch (error) {
        clearTimeout(connectTimer);
        lastError = error;
        const isConnectTimeout = connectCtrl.signal.aborted && error.name === "AbortError";
        dbg("FETCH", `${this.provider.toUpperCase()} ✖ ${error.name}: ${error.message}${isConnectTimeout ? " (connect timeout)" : ""}`);
        // Connect timeout is internal — convert to retryable network error, don't propagate AbortError
        if (error.name === "AbortError" && !isConnectTimeout) throw error;

        // Map network/fetch exceptions to 502 retry config
        if (await tryRetry(urlIndex, HTTP_STATUS.BAD_GATEWAY, `network "${error.message}"`)) { urlIndex--; continue; }

        if (urlIndex + 1 < fallbackCount) {
          log?.debug?.("RETRY", `Error on ${url}, trying fallback ${urlIndex + 1}`);
          continue;
        }
        throw error;
      }
    }

    throw lastError || new Error(`All ${fallbackCount} URLs failed with status ${lastStatus}`);
  }
}

export default BaseExecutor;
