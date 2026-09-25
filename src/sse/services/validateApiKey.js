import { getSettings, getApiKeyByKey, updateApiKey, updateSettings } from "@/lib/localDb";
import { extractApiKey, isValidApiKey } from "./auth.js";
import { enforceApiKeyRestrictions } from "./apiKeyEnforcement.js";
import { errorResponse } from "open-sse/utils/error.js";
import { HTTP_STATUS } from "open-sse/config/runtimeConfig.js";
import * as log from "../utils/logger.js";
import { budgetHeaders } from "@/lib/budgetService.js";
import { isTrustedInternalRequest } from "@/lib/auth/internalRequest.js";
import { trustedClientIp } from "@/lib/auth/clientIp.js";
import { recordMetric } from "@/lib/observability/metrics.js";

// In-memory track of rate limit violations per IP for auto-blocking
const ipViolationCounts = new Map();
const autoBlockThreshold = 10; // Block IP after this many rate limit violations
const violationWindowMs = 300_000; // 5 minute window for counting violations

function trackIpViolation(ip) {
  if (!ip) return;
  const now = Date.now();
  const entry = ipViolationCounts.get(ip);
  if (!entry || now - entry.windowStart > violationWindowMs) {
    ipViolationCounts.set(ip, { count: 1, windowStart: now });
  } else {
    entry.count++;
    if (entry.count >= autoBlockThreshold) {
      return true; // Should auto-block
    }
  }
  return false;
}

/**
 * Validate API key and enforce restrictions (rate limits, IP blocks, model restrictions, token limits)
 * @param {Request} request - The incoming request
 * @param {object} body - The parsed request body
 * @param {string} handlerName - Name of the handler for logging (e.g., "CHAT", "SEARCH")
 * @returns {{ allowed: boolean, response?: Response, apiKey?: string }}
 */
export async function validateAndEnforceApiKey(request, body, handlerName = "AUTH") {
  const internal = await isTrustedInternalRequest(request);
  const settings = await getSettings();
  const gatewayControl = settings.gatewayControl || {};
  const requestModel = body?.model || body?.provider || "";
  const requestProvider = String(requestModel).split("/")[0];
  if (!internal && gatewayControl.disabled) {
    recordMetric("gateway_kill_switch_total", 1, { reason: gatewayControl.reason || "disabled" });
    return { allowed: false, response: errorResponse(HTTP_STATUS.SERVICE_UNAVAILABLE, gatewayControl.reason || "Gateway is disabled") };
  }
  if (!internal && gatewayControl.drain) {
    return { allowed: false, response: errorResponse(HTTP_STATUS.SERVICE_UNAVAILABLE, "Gateway is draining and not accepting new requests", ) };
  }
  if (!internal && (gatewayControl.blockedModels || []).includes(requestModel)) {
    return { allowed: false, response: errorResponse(HTTP_STATUS.FORBIDDEN, "Model is disabled by gateway control") };
  }
  if (!internal && requestProvider && (gatewayControl.blockedProviders || []).includes(requestProvider)) {
    return { allowed: false, response: errorResponse(HTTP_STATUS.FORBIDDEN, "Provider is disabled by gateway control") };
  }
  if (internal) return { allowed: true, apiKey: null, keyRecord: null, release: null };

  const apiKey = extractApiKey(request);
  const clientIp = trustedClientIp(request);

  let keyRecord = null;
  if (apiKey) {
    try {
      keyRecord = await getApiKeyByKey(apiKey);
    } catch {
      keyRecord = null;
    }
  }

  if (!settings.requireApiKey) {
    if (!keyRecord) return { allowed: true, apiKey, keyRecord: null };
    if (!(await isValidApiKey(apiKey))) {
      return { allowed: false, response: errorResponse(HTTP_STATUS.UNAUTHORIZED, "Invalid API key") };
    }
  } else {
    if (!apiKey) {
      log.warn(handlerName, "Missing API key (requireApiKey=true)");
      return { allowed: false, response: errorResponse(HTTP_STATUS.UNAUTHORIZED, "Missing API key") };
    }

    const valid = await isValidApiKey(apiKey);
    if (!valid) {
      log.warn(handlerName, "Invalid API key (requireApiKey=true)");
      return { allowed: false, response: errorResponse(HTTP_STATUS.UNAUTHORIZED, "Invalid API key") };
    }
  }

  // Enforce per-key restrictions (rate limits, IP blocks, model restrictions, token limits)
  const restrictionCheck = apiKey && (settings.requireApiKey || keyRecord)
    ? await enforceApiKeyRestrictions(apiKey, request, body)
    : { allowed: true };
  if (!restrictionCheck.allowed) {
    log.warn(handlerName, `API key restriction denied: ${restrictionCheck.error}`);

    // Build response with rate limit headers
    const res = restrictionCheck.status === 402
      ? new Response(JSON.stringify({
          error: {
            message: restrictionCheck.error,
            type: "budget_exceeded",
            param: null,
            code: 402,
          },
        }), {
          status: 402,
          headers: { "Content-Type": "application/json", "Access-Control-Allow-Origin": "*" },
        })
      : errorResponse(restrictionCheck.status || 403, restrictionCheck.error);

    for (const [name, value] of Object.entries(budgetHeaders(restrictionCheck.budget))) {
      res.headers.set(name, value);
    }
    if (restrictionCheck.status === 402 && restrictionCheck.budget?.resetsAt) {
      const resetMs = new Date(restrictionCheck.budget.resetsAt).getTime();
      if (Number.isFinite(resetMs)) {
        res.headers.set("Retry-After", String(Math.max(Math.ceil((resetMs - Date.now()) / 1000), 1)));
      }
    }

    if (restrictionCheck.retryAfter) {
      res.headers.set("Retry-After", String(restrictionCheck.retryAfter));
    }

    // Add standard rate limit headers
    if (keyRecord && keyRecord.rateLimitRequests > 0) {
      res.headers.set("X-RateLimit-Limit", String(keyRecord.rateLimitRequests));
      res.headers.set("X-RateLimit-Remaining", "0");
      res.headers.set("X-RateLimit-Reset", String(Math.ceil(Date.now() / 1000) + (keyRecord.rateLimitWindowSeconds || 60)));
    }

    // Auto-block abusive IPs
    if (restrictionCheck.status === 429 && clientIp) {
      const shouldBlock = trackIpViolation(clientIp);
      if (shouldBlock) {
        log.warn(handlerName, `Auto-blocking IP ${clientIp} after ${autoBlockThreshold} rate limit violations`);
        // Store blocked IP in settings
        try {
          const currentSettings = await getSettings();
          const blockedIPs = currentSettings.autoBlockedIPs || [];
          if (!blockedIPs.includes(clientIp)) {
            blockedIPs.push(clientIp);
            await updateSettings({ autoBlockedIPs: blockedIPs });
          }
        } catch (e) {
          log.error(handlerName, `Failed to auto-block IP: ${e.message}`);
        }
      }
    }

    recordMetric("api_key_denied_total", 1, { status: String(restrictionCheck.status || 403), reason: restrictionCheck.error || "restriction" });
    await restrictionCheck.release?.();
    return { allowed: false, response: res, release: null };
  }

  // Check if IP is auto-blocked
  if (clientIp) {
    try {
      const currentSettings = await getSettings();
      const autoBlockedIPs = currentSettings.autoBlockedIPs || [];
      if (autoBlockedIPs.includes(clientIp)) {
        log.warn(handlerName, `IP ${clientIp} is auto-blocked`);
        await restrictionCheck.release?.();
        return {
          allowed: false,
          response: errorResponse(HTTP_STATUS.FORBIDDEN, "Your IP has been temporarily blocked due to excessive rate limit violations. Please try again later.")
        };
      }
    } catch (e) {
      // Ignore settings read errors
    }
  }

  recordMetric("api_key_allowed_total", 1, { model: requestModel || "unknown" });
  return { allowed: true, apiKey, keyRecord, release: restrictionCheck.release || null };
}
