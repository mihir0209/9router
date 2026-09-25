import { getApiKeyByKey, getCombos } from "@/lib/localDb";
import { getModelInfo } from "@/sse/services/model.js";
import { getApiKeyTokenUsage } from "@/lib/db/repos/usageRepo.js";
import { checkBudget } from "@/lib/budgetService.js";
import { coordinatorCall } from "@/lib/coordinator/client.js";
import { ipMatchesAny, trustedClientIp } from "@/lib/auth/clientIp.js";

function endpointFamily(pathname) {
  const path = String(pathname || "").replace(/^\/api(?=\/v1(?:\/|$))/, "");
  if (/^\/v1\/chat\/completions\/?$/.test(path) || /^\/chat\/completions\/?$/.test(path)) return "chat.completions";
  if (/^\/v1\/messages\/?$/.test(path) || /^\/messages\/?$/.test(path)) return "messages";
  if (/^\/(?:v1\/)?responses\/?$/.test(path)) return "responses";
  if (/^\/v1\/embeddings\/?$/.test(path) || /^\/embeddings\/?$/.test(path)) return "embeddings";
  if (/^\/v1\/audio\//.test(path) || /^\/audio\//.test(path)) return "audio";
  if (/^\/v1\/images\//.test(path) || /^\/images\//.test(path)) return "images";
  if (/^\/v1\/videos\//.test(path) || /^\/videos\//.test(path)) return "videos";
  if (/^\/v1\/web_search\/?$/.test(path) || /^\/web_search\/?$/.test(path)) return "web_search";
  if (/^\/v1\/web_fetch\/?$/.test(path) || /^\/web_fetch\/?$/.test(path)) return "web_fetch";
  return path.replace(/\/$/, "") || "/";
}

function parseList(str) {
  if (!str || typeof str !== "string") return [];
  return str.split(",").map((item) => item.trim()).filter(Boolean);
}

async function resolveRequestedModels(model, body) {
  const raw = String(model || body?.provider || "").trim();
  if (!raw) return { models: [], providers: [] };
  const combo = (await getCombos()).find((entry) => entry.name === raw);
  const requested = combo?.models?.length ? combo.models : [raw];
  const resolved = [];
  for (const value of requested) {
    const text = String(value).trim();
    const info = await getModelInfo(text).catch(() => ({ provider: null, model: null }));
    if (info?.provider) resolved.push({ requested: text, provider: info.provider, model: info.model });
    else if (text.includes("/")) resolved.push({ requested: text, provider: text.split("/")[0], model: text.split("/").slice(1).join("/") });
  }
  return {
    models: resolved.flatMap((entry) => [entry.requested, `${entry.provider}/${entry.model}`]),
    providers: [...new Set(resolved.map((entry) => entry.provider).filter(Boolean))],
  };
}

export async function enforceApiKeyRestrictions(apiKey, request, body) {
  if (!apiKey) return { allowed: true };
  const keyRecord = await getApiKeyByKey(apiKey);
  if (!keyRecord) return { allowed: false, error: "Invalid API key", status: 401 };
  if (!keyRecord.isActive) return { allowed: false, error: "API key is disabled", status: 401 };
  if (keyRecord.expiresAt) {
    const expiresAt = new Date(keyRecord.expiresAt);
    if (Number.isFinite(expiresAt.getTime()) && expiresAt <= new Date()) return { allowed: false, error: "API key has expired", status: 401 };
  }

  const model = body?.model;
  const resolvedRequest = await resolveRequestedModels(model, body);
  const requestedModels = new Set(resolvedRequest.models);
  if (model) {
    const allowedModels = parseList(keyRecord.allowedModels);
    if (allowedModels.length > 0 && !allowedModels.includes("*") && ![...requestedModels].some((entry) => allowedModels.includes(entry))) return { allowed: false, error: `Model '${model}' is not allowed for this API key`, status: 403 };
    if (parseList(keyRecord.blockedModels).some((entry) => requestedModels.has(entry))) return { allowed: false, error: `Model '${model}' is blocked for this API key`, status: 403 };
  }

  const providers = resolvedRequest.providers;
  const allowedProviders = parseList(keyRecord.allowedProviders);
  const blockedProviders = parseList(keyRecord.blockedProviders);
  if (allowedProviders.length > 0 && (!providers.length || providers.some((provider) => !allowedProviders.includes(provider) && !allowedProviders.includes("*")))) return { allowed: false, error: "Provider is not allowed for this API key", status: 403 };
  if (providers.some((provider) => blockedProviders.includes(provider))) return { allowed: false, error: "Provider is blocked for this API key", status: 403 };

  const endpoint = new URL(request.url || "http://localhost").pathname;
  const family = endpointFamily(endpoint);
  const allowedEndpoints = parseList(keyRecord.allowedEndpoints);
  const blockedEndpoints = parseList(keyRecord.blockedEndpoints);
  const endpointAllowed = (rule) => rule === "*" || rule === endpoint || endpointFamily(rule) === family;
  if (allowedEndpoints.length > 0 && !allowedEndpoints.some(endpointAllowed)) return { allowed: false, error: "Endpoint is not allowed for this API key", status: 403 };
  if (blockedEndpoints.some(endpointAllowed)) return { allowed: false, error: "Endpoint is blocked for this API key", status: 403 };

  const clientIp = trustedClientIp(request);
  const blockedIPs = parseList(keyRecord.blockedIPs);
  const allowedIPs = parseList(keyRecord.allowedIPs);
  if ((blockedIPs.length > 0 || allowedIPs.length > 0) && !clientIp) return { allowed: false, error: "Client IP could not be verified", status: 403 };
  if (blockedIPs.length > 0 && ipMatchesAny(clientIp, blockedIPs)) return { allowed: false, error: "IP address is blocked", status: 403 };
  if (allowedIPs.length > 0 && !ipMatchesAny(clientIp, allowedIPs)) return { allowed: false, error: "IP address not in allowlist", status: 403 };

  if (keyRecord.rateLimitRequests > 0) {
    const now = Date.now();
    const result = await coordinatorCall("checkRateLimit", { keyId: keyRecord.id, limit: keyRecord.rateLimitRequests, windowMs: (keyRecord.rateLimitWindowSeconds || 60) * 1000, now }, `tenant:${keyRecord.ownerRef || keyRecord.id}`, { failClosed: true });
    if (!result.allowed) return { allowed: false, error: `Rate limit exceeded. Max ${keyRecord.rateLimitRequests} requests per ${keyRecord.rateLimitWindowSeconds}s`, status: 429, retryAfter: Math.max(1, Math.ceil((result.resetAt - now) / 1000)) };
  }

  let lease = null;
  if (keyRecord.maxConcurrentRequests > 0) {
    const result = await coordinatorCall("beginRequest", { keyId: keyRecord.id, maxConcurrent: keyRecord.maxConcurrentRequests }, `tenant:${keyRecord.ownerRef || keyRecord.id}`, { failClosed: true });
    if (!result.allowed) return { allowed: false, error: "Concurrent request limit reached", status: 429, retryAfter: 2 };
    lease = result.leaseId;
  }

  const budget = await checkBudget(apiKey);
  if (budget.exceeded) {
    if (lease) await coordinatorCall("endRequest", { keyId: keyRecord.id, leaseId: lease }, `tenant:${keyRecord.ownerRef || keyRecord.id}`, { failClosed: true });
    return { allowed: false, error: "Budget exceeded for workspace key. Add credits to continue.", status: 402, budget };
  }

  const sinceDaily = new Date();
  sinceDaily.setUTCHours(0, 0, 0, 0);
  if (keyRecord.tokenLimitDaily > 0) {
    const total = await getApiKeyTokenUsage(keyRecord.id, apiKey, sinceDaily);
    if (total >= keyRecord.tokenLimitDaily) return { allowed: false, error: `Daily token limit reached (${keyRecord.tokenLimitDaily})`, status: 429, release: lease };
  }
  if (keyRecord.tokenLimitMonthly > 0) {
    const total = await getApiKeyTokenUsage(keyRecord.id, apiKey, new Date(Date.now() - 30 * 24 * 60 * 60 * 1000));
    if (total >= keyRecord.tokenLimitMonthly) return { allowed: false, error: `Monthly token limit reached (${keyRecord.tokenLimitMonthly})`, status: 429, release: lease };
  }

  return { allowed: true, lease, release: lease ? async () => coordinatorCall("endRequest", { keyId: keyRecord.id, leaseId: lease }, `tenant:${keyRecord.ownerRef || keyRecord.id}`, { failClosed: true }) : null };
}
