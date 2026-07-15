import { getApiKeyByKey } from "@/lib/localDb";
import { getUsageStats } from "@/lib/db/repos/usageRepo.js";

// In-memory rate limit tracking: Map<keyId, { count, windowStart }>
const rateLimitStore = new Map();

// Cleanup stale entries every 5 minutes
setInterval(() => {
  const now = Date.now();
  for (const [keyId, entry] of rateLimitStore) {
    if (now - entry.windowStart > 120_000) {
      rateLimitStore.delete(keyId);
    }
  }
}, 300_000);

/**
 * Parse comma-separated list, trim, filter empty
 */
function parseList(str) {
  if (!str || typeof str !== "string") return [];
  return str.split(",").map(s => s.trim()).filter(Boolean);
}

/**
 * Check if an IP is in a list (supports CIDR notation for simple ranges)
 */
function ipMatches(ip, list) {
  for (const entry of list) {
    if (entry === ip) return true;
    if (entry.includes("/")) {
      const [subnet, bits] = entry.split("/");
      const mask = ~((1 << (32 - parseInt(bits))) - 1);
      const ipNum = ipToNum(ip);
      const subNum = ipToNum(subnet);
      if (ipNum !== null && subNum !== null && (ipNum & mask) === (subNum & mask)) return true;
    }
  }
  return false;
}

function ipToNum(ip) {
  const parts = ip.split(".").map(Number);
  if (parts.length !== 4 || parts.some(p => isNaN(p) || p < 0 || p > 255)) return null;
  return ((parts[0] << 24) | (parts[1] << 16) | (parts[2] << 8) | parts[3]) >>> 0;
}

/**
 * Enforce API key restrictions: IP blocks, model allow/block, rate limits, token limits, expiry
 * Returns { allowed: boolean, error?: string, status?: number }
 */
export async function enforceApiKeyRestrictions(apiKey, request, body) {
  if (!apiKey) return { allowed: true };

  const keyRecord = await getApiKeyByKey(apiKey);
  if (!keyRecord) return { allowed: false, error: "Invalid API key", status: 401 };
  if (!keyRecord.isActive) return { allowed: false, error: "API key is disabled", status: 401 };

  // Expiry check
  if (keyRecord.expiresAt) {
    const expiresAt = new Date(keyRecord.expiresAt);
    if (!isNaN(expiresAt.getTime()) && expiresAt < new Date()) {
      return { allowed: false, error: "API key has expired", status: 401 };
    }
  }

  // IP blocking
  const clientIp = request.headers?.get("x-9r-real-ip") || "";
  const blockedIPs = parseList(keyRecord.blockedIPs);
  if (blockedIPs.length > 0 && clientIp && ipMatches(clientIp, blockedIPs)) {
    return { allowed: false, error: "IP address is blocked", status: 403 };
  }

  // IP allowlist (if set, only these IPs are allowed)
  const allowedIPs = parseList(keyRecord.allowedIPs);
  if (allowedIPs.length > 0 && clientIp && !ipMatches(clientIp, allowedIPs)) {
    return { allowed: false, error: "IP address not in allowlist", status: 403 };
  }

  // Model restrictions
  const model = body?.model;
  if (model) {
    const allowedModels = parseList(keyRecord.allowedModels);
    if (allowedModels.length > 0 && !allowedModels.includes(model) && !allowedModels.includes("*")) {
      return { allowed: false, error: `Model '${model}' is not allowed for this API key`, status: 403 };
    }
    const blockedModels = parseList(keyRecord.blockedModels);
    if (blockedModels.length > 0 && blockedModels.includes(model)) {
      return { allowed: false, error: `Model '${model}' is blocked for this API key`, status: 403 };
    }
  }

  // Rate limiting (requests per window)
  if (keyRecord.rateLimitRequests > 0) {
    const keyId = keyRecord.id;
    const windowMs = (keyRecord.rateLimitWindowSeconds || 60) * 1000;
    const now = Date.now();
    const entry = rateLimitStore.get(keyId);

    if (!entry || now - entry.windowStart > windowMs) {
      rateLimitStore.set(keyId, { count: 1, windowStart: now });
    } else {
      entry.count++;
      if (entry.count > keyRecord.rateLimitRequests) {
        const retryAfter = Math.ceil((entry.windowStart + windowMs - now) / 1000);
        return {
          allowed: false,
          error: `Rate limit exceeded. Max ${keyRecord.rateLimitRequests} requests per ${keyRecord.rateLimitWindowSeconds}s`,
          status: 429,
          retryAfter,
        };
      }
    }
  }

  // Token daily limit
  if (keyRecord.tokenLimitDaily > 0) {
    const today = new Date().toISOString().slice(0, 10);
    const stats = await getUsageStats("today");
    const keyUsage = stats?.byApiKey?.[apiKey] || stats?.byApiKey?.[`${apiKey}|${model}|*`] || {};
    const totalTokensToday = (keyUsage.promptTokens || 0) + (keyUsage.completionTokens || 0);
    if (totalTokensToday >= keyRecord.tokenLimitDaily) {
      return {
        allowed: false,
        error: `Daily token limit reached (${keyRecord.tokenLimitDaily})`,
        status: 429,
      };
    }
  }

  // Token monthly limit
  if (keyRecord.tokenLimitMonthly > 0) {
    const stats = await getUsageStats("30d");
    const keyUsage = stats?.byApiKey?.[apiKey] || {};
    const totalTokensMonth = (keyUsage.promptTokens || 0) + (keyUsage.completionTokens || 0);
    if (totalTokensMonth >= keyRecord.tokenLimitMonthly) {
      return {
        allowed: false,
        error: `Monthly token limit reached (${keyRecord.tokenLimitMonthly})`,
        status: 429,
      };
    }
  }

  return { allowed: true };
}
