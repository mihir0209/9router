import { getSettings, getApiKeyByKey, updateApiKey } from "@/lib/localDb";
import { extractApiKey, isValidApiKey } from "./auth.js";
import { enforceApiKeyRestrictions } from "./apiKeyEnforcement.js";
import { errorResponse } from "open-sse/utils/error.js";
import { HTTP_STATUS } from "open-sse/config/runtimeConfig.js";
import * as log from "../utils/logger.js";

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
  const settings = await getSettings();
  const apiKey = extractApiKey(request);
  const clientIp = request.headers?.get("x-9r-real-ip") || "";

  if (!settings.requireApiKey) {
    return { allowed: true, apiKey };
  }

  if (!apiKey) {
    log.warn(handlerName, "Missing API key (requireApiKey=true)");
    return { allowed: false, response: errorResponse(HTTP_STATUS.UNAUTHORIZED, "Missing API key") };
  }

  const valid = await isValidApiKey(apiKey);
  if (!valid) {
    log.warn(handlerName, "Invalid API key (requireApiKey=true)");
    return { allowed: false, response: errorResponse(HTTP_STATUS.UNAUTHORIZED, "Invalid API key") };
  }

  // Enforce per-key restrictions (rate limits, IP blocks, model restrictions, token limits)
  const restrictionCheck = await enforceApiKeyRestrictions(apiKey, request, body);
  if (!restrictionCheck.allowed) {
    log.warn(handlerName, `API key restriction denied: ${restrictionCheck.error}`);

    // Build response with rate limit headers
    const res = errorResponse(restrictionCheck.status || 403, restrictionCheck.error);

    if (restrictionCheck.retryAfter) {
      res.headers.set("Retry-After", String(restrictionCheck.retryAfter));
    }

    // Add standard rate limit headers
    const keyRecord = await getApiKeyByKey(apiKey);
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

  return { allowed: true, apiKey };
}
