import { getSettings, getApiKeyByKey, updateApiKey } from "@/lib/localDb";
import { extractApiKey, isValidApiKey } from "./auth.js";
import { enforceApiKeyRestrictions } from "./apiKeyEnforcement.js";
import { errorResponse } from "open-sse/utils/error.js";
import { HTTP_STATUS } from "open-sse/config/runtimeConfig.js";
import * as log from "../utils/logger.js";
import { budgetHeaders } from "@/lib/budgetService.js";

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
  if (request.headers?.get("x-9r-dashboard-internal") === "1") {
    return { allowed: true, apiKey: null, keyRecord: null };
  }

  const settings = await getSettings();
  const apiKey = extractApiKey(request);
  const clientIp = request.headers?.get("x-9r-real-ip") || "";

  let keyRecord = apiKey ? await getApiKeyByKey(apiKey) : null;

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
  const restrictionCheck = await enforceApiKeyRestrictions(apiKey, request, body);
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

    return { allowed: false, response: res };
  }

  // Check if IP is auto-blocked
  if (clientIp) {
    try {
      const currentSettings = await getSettings();
      const autoBlockedIPs = currentSettings.autoBlockedIPs || [];
      if (autoBlockedIPs.includes(clientIp)) {
        log.warn(handlerName, `IP ${clientIp} is auto-blocked`);
        return {
          allowed: false,
          response: errorResponse(HTTP_STATUS.FORBIDDEN, "Your IP has been temporarily blocked due to excessive rate limit violations. Please try again later.")
        };
      }
    } catch (e) {
      // Ignore settings read errors
    }
  }

  return { allowed: true, apiKey, keyRecord };
}
