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

// The coordinator expires a lease after LEASE_TTL_MS so a crashed isolate
// cannot hold a slot forever. A legitimately long stream outlives that, so the
// lease is renewed on a timer; releasing stops the timer and ends the lease.
const LEASE_TTL_MS = 120_000;
const LEASE_RENEW_MS = Math.floor(LEASE_TTL_MS / 3);

function createLeaseRelease({ keyId, leaseId, scope }) {
  let done = false;
  const renew = setInterval(() => {
    void coordinatorCall("renewRequest", { keyId, leaseId, ttlMs: LEASE_TTL_MS }, scope, { failClosed: true }).catch(() => {});
  }, LEASE_RENEW_MS);
  renew.unref?.();

  return async () => {
    if (done) return;
    done = true;
    clearInterval(renew);
    await coordinatorCall("endRequest", { keyId, leaseId }, scope, { failClosed: true });
  };
}

// A combo may list another combo as a member, and those nest arbitrarily deep.
// Bound the walk so a cycle (or a pathological chain) cannot hang a request.
const COMBO_EXPANSION_DEPTH = 10;

async function resolveRequestedModels(model, body) {
  const raw = String(model || body?.provider || "").trim();
  if (!raw) return { models: [], providers: [], isCombo: false, leaves: [] };

  const combos = await getCombos();
  const rootCombo = combos.find((entry) => entry.name === raw);

  // Depth-first over combo members, with a visited set: a combo reachable twice
  // is expanded once, and a cycle terminates instead of recursing forever.
  const leaves = [];
  const visited = new Set();
  const walk = async (names, depth) => {
    if (depth > COMBO_EXPANSION_DEPTH) return;
    for (const name of names) {
      const text = String(name).trim();
      if (!text || visited.has(text)) continue;
      visited.add(text);
      const nested = combos.find((entry) => entry.name === text);
      if (nested?.models?.length) await walk(nested.models, depth + 1);
      else leaves.push(text);
    }
  };
  await walk(rootCombo?.models?.length ? rootCombo.models : [raw], 0);

  const resolved = [];
  for (const text of leaves) {
    const info = await getModelInfo(text).catch(() => ({ provider: null, model: null }));
    if (info?.provider) resolved.push({ requested: text, provider: info.provider, model: info.model });
    else if (text.includes("/")) resolved.push({ requested: text, provider: text.split("/")[0], model: text.split("/").slice(1).join("/") });
  }
  return {
    models: resolved.flatMap((entry) => [entry.requested, `${entry.provider}/${entry.model}`]),
    providers: [...new Set(resolved.map((entry) => entry.provider).filter(Boolean))],
    isCombo: Boolean(rootCombo?.models?.length),
    leaves: [...new Set(leaves)],
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
    if (allowedModels.length > 0 && !allowedModels.includes("*")) {
      // 1. The id the client actually asked for wins. A key that lists a combo
      //    name may call that combo, which is the intuitive admin semantic and
      //    the only thing that makes such a key usable at all.
      const rawAllowed = allowedModels.includes(String(model).trim());
      // 3. For a concrete request, fall back to the resolved provider/model so
      //    existing configs keep working (e.g. asking for `gpt-4o` against an
      //    allow-list entry of `openai/gpt-4o`).
      //
      //    Deliberately NOT applied to combo requests. The resolved set of a
      //    combo is every model it can reach, so matching it with .some() would
      //    silently widen a key restricted to ONE member of a combo into access
      //    to ALL of them. A combo is authorised by name, not by its members.
      const resolvedAllowed = !resolvedRequest.isCombo
        && [...requestedModels].some((entry) => allowedModels.includes(entry));
      if (!rawAllowed && !resolvedAllowed) {
        // Log what the request actually expands to: the 403 quotes the raw ask
        // while the decision was made on the resolved list, which is what made
        // this so hard to diagnose from the client side.
        console.log(`[apiKey] model denied: asked="${model}" leaves=${JSON.stringify(resolvedRequest.leaves)}`);
        return { allowed: false, error: `Model '${model}' is not allowed for this API key`, status: 403 };
      }
    }
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

  let release = null;
  if (keyRecord.maxConcurrentRequests > 0) {
    const scope = `tenant:${keyRecord.ownerRef || keyRecord.id}`;
    const result = await coordinatorCall("beginRequest", { keyId: keyRecord.id, maxConcurrent: keyRecord.maxConcurrentRequests }, scope, { failClosed: true });
    if (!result.allowed) return { allowed: false, error: "Concurrent request limit reached", status: 429, retryAfter: 2 };
    release = createLeaseRelease({ keyId: keyRecord.id, leaseId: result.leaseId, scope });
  }

  const endLease = () => (release ? release() : Promise.resolve());

  const budget = await checkBudget(apiKey);
  if (budget.exceeded) {
    await endLease();
    return { allowed: false, error: "Budget exceeded for workspace key. Add credits to continue.", status: 402, budget };
  }

  const sinceDaily = new Date();
  sinceDaily.setUTCHours(0, 0, 0, 0);
  if (keyRecord.tokenLimitDaily > 0) {
    const total = await getApiKeyTokenUsage(keyRecord.id, apiKey, sinceDaily);
    if (total >= keyRecord.tokenLimitDaily) {
      await endLease();
      return { allowed: false, error: `Daily token limit reached (${keyRecord.tokenLimitDaily})`, status: 429 };
    }
  }
  if (keyRecord.tokenLimitMonthly > 0) {
    const total = await getApiKeyTokenUsage(keyRecord.id, apiKey, new Date(Date.now() - 30 * 24 * 60 * 60 * 1000));
    if (total >= keyRecord.tokenLimitMonthly) {
      await endLease();
      return { allowed: false, error: `Monthly token limit reached (${keyRecord.tokenLimitMonthly})`, status: 429 };
    }
  }

  return { allowed: true, lease: release ? true : null, release };
}
