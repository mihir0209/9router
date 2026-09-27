import { PROVIDER_MODELS, PROVIDER_ID_TO_ALIAS, getModelKind } from "@/shared/constants/models";
import {
  AI_PROVIDERS,
  getProviderAlias,
  isAnthropicCompatibleProvider,
  isOpenAICompatibleProvider,
} from "@/shared/constants/providers";
import { getProviderConnections, getCombos, getCustomModels, getModelAliases } from "@/lib/localDb";
import { getDisabledModels } from "@/lib/disabledModelsDb";
import { capabilitiesFromServiceKind, getCapabilitiesForModel, aggregateComboCapabilities } from "open-sse/providers/capabilities.js";
import { mapWithConcurrency } from "@/lib/async/mapWithConcurrency.js";
import { resolveProviderModelList, INTERNAL_MODELS_FETCH_HEADER } from "@/lib/modelCatalog/providerModelResolver.js";
import { getSettings } from "@/lib/localDb";
import { normalizeModelsListSource } from "@/lib/db/repos/settingsRepo.js";


// LLM kind sentinel — combos/models with no explicit kind default to LLM
const LLM_KIND = "llm";
const CLOUDFLARE_STATIC_PROVIDER_BLOCKLIST = new Set(["opencode"]);
const MODEL_PROVIDER_CONCURRENCY = 4;
const MODEL_PROVIDER_TIMEOUT_MS = 6000;

// Pre-serialized /v1/models body. Rebuilding capabilities for thousands of
// models blows the Workers CPU budget (Cloudflare error 1102 → 503); serving
// a cached string keeps warm requests near-zero CPU.
// isolate-local (fast path) + Cache API on Workers (shared across isolates).
const MODELS_LIST_TTL_MS = 300_000;
const MODELS_EDGE_TTL_S = 300;
if (!global._modelsListCache) {
  global._modelsListCache = { body: null, ts: 0, inflight: null };
}
const modelsListCache = global._modelsListCache;
const isCloudflareRuntime = () => process.env.NINEROUTER_RUNTIME === "cloudflare";

/**
 * Resolve the model list source, tolerating a failed settings read.
 * A settings hiccup must degrade to the default rather than 500 the whole
 * model list, so this never throws.
 */
async function resolveModelsListSource(explicit) {
  if (explicit) return normalizeModelsListSource(explicit);
  try {
    const settings = await getSettings();
    return normalizeModelsListSource(settings?.modelsListSource);
  } catch (err) {
    console.log(`Could not read modelsListSource, using default: ${err?.message || err}`);
    return normalizeModelsListSource(undefined);
  }
}

function modelsListHeaders(extra = {}) {
  return {
    "Content-Type": "application/json; charset=utf-8",
    "Access-Control-Allow-Origin": "*",
    "Cache-Control": "public, max-age=30",
    ...extra,
  };
}

function modelsEdgeCacheKey(request) {
  // Auth-agnostic: the catalog shape does not depend on the caller's key.
  return new Request(new URL("/v1/models", request.url).href, {
    method: "GET",
    headers: { accept: "application/json" },
  });
}

async function readModelsEdgeCache(request) {
  if (!isCloudflareRuntime()) return null;
  try {
    const hit = await caches.default.match(modelsEdgeCacheKey(request));
    if (!hit) return null;
    const body = await hit.text();
    return body || null;
  } catch {
    return null;
  }
}

async function writeModelsEdgeCache(request, body) {
  if (!isCloudflareRuntime() || !body) return;
  try {
    const res = new Response(body, {
      headers: modelsListHeaders({
        "Cache-Control": `public, max-age=${MODELS_EDGE_TTL_S}`,
      }),
    });
    await caches.default.put(modelsEdgeCacheKey(request), res);
  } catch {
    /* edge cache is best-effort */
  }
}

// Map per-model `type` field (in PROVIDER_MODELS) to service kind.
// Models without `type` are treated as LLM.
const MODEL_TYPE_TO_KIND = {
  image: "image",
  tts: "tts",
  embedding: "embedding",
  stt: "stt",
  imageToText: "imageToText",
  video: "video",
};

function modelKind(model) {
  const k = model?.kind || model?.type;
  if (!k) return LLM_KIND;
  return MODEL_TYPE_TO_KIND[k] || LLM_KIND;
}

// For dynamic/unknown model IDs (compatible providers, alias map, custom models)
// fall back to provider-level kind matching when per-model type is unavailable.
function inferKindFromUnknownModelId(modelId) {
  const lower = String(modelId).toLowerCase();
  if (/embed/.test(lower)) return "embedding";
  if (/tts|speech|audio|voice/.test(lower)) return "tts";
  if (/image|imagen|dall-?e|flux|sdxl|sd-|stable-diffusion/.test(lower)) return "image";
  return LLM_KIND;
}

function stripProviderPrefix(modelId, aliases) {
  const value = typeof modelId === "string" ? modelId.trim() : "";
  if (!value) return "";
  for (const alias of aliases) {
    if (alias && value.startsWith(`${alias}/`)) return value.slice(alias.length + 1);
  }
  return value;
}

// Provider matches kindFilter when its serviceKinds intersect the requested kinds.
// LLM is the default kind for providers missing serviceKinds.
function providerMatchesKinds(providerId, kindFilter, connection = null) {
  const customMediaKind = connection?.providerSpecificData?.mediaKind;
  if (customMediaKind) return kindFilter.includes(customMediaKind);
  const provider = AI_PROVIDERS[providerId];
  const kinds = Array.isArray(provider?.serviceKinds) && provider.serviceKinds.length > 0
    ? provider.serviceKinds
    : [LLM_KIND];
  return kindFilter.some((k) => kinds.includes(k));
}

function shouldExposeStaticProvider(providerId) {
  if (process.env.NINEROUTER_RUNTIME !== "cloudflare") return true;
  return !CLOUDFLARE_STATIC_PROVIDER_BLOCKLIST.has(providerId);
}

// Combo matches kindFilter when its `kind` field is in the list.
// Combos with no kind are treated as LLM.
function comboMatchesKinds(combo, kindFilter) {
  const kind = combo?.kind || LLM_KIND;
  return kindFilter.includes(kind);
}

/**
 * Build OpenAI-format models list filtered by service kinds.
 * @param {string[]} kindFilter - List of service kinds to include (e.g. ["llm"], ["webSearch","webFetch"]).
 */
export async function buildModelsList(kindFilter, options = {}) {
  // When this header is present, the /v1/models request came from another
  // 9router instance's fetchCompatibleModelIds — skip dynamic fetch to break
  // cross-instance recursive loops.
  // On Workers, also skip live catalog / upstream discovery: they burn CPU
  // past the free-plan budget (error 1102) while building thousands of entries.
  // Where the model list comes from:
  //   "live"   — ask each provider (one upstream round trip per provider).
  //   "cached" — serve the DB copy, refreshed via POST /api/models/refresh.
  // Env default via MODELS_LIST_SOURCE, overridable in the settings page.
  // Cloudflare used to hard-force the cached path because that upstream work
  // blew the free-plan CPU budget; that is now a choice instead of a hardcode,
  // and "cached" remains the recommended setting there.
  const modelsListSource = await resolveModelsListSource(options.modelsListSource);
  const allowLive = modelsListSource === "live" && options.skipDynamicFetch !== true && !isCloudflareRuntime();
  const providerConcurrency = options.providerConcurrency || MODEL_PROVIDER_CONCURRENCY;
  const providerTimeoutMs = options.providerTimeoutMs || MODEL_PROVIDER_TIMEOUT_MS;
  const dbReads = await Promise.allSettled([
    getProviderConnections(),
    getCombos(),
    getCustomModels(),
    getModelAliases(),
    getDisabledModels(),
  ]);
  const [connectionsResult, combosResult, customModelsResult, aliasesResult, disabledResult] = dbReads;
  const providerReadFailed = connectionsResult.status === "rejected";
  if (providerReadFailed) console.log("Could not fetch providers, returning static fallback models");
  if (combosResult.status === "rejected") console.log("Could not fetch combos");
  if (customModelsResult.status === "rejected") console.log("Could not fetch custom models");
  if (aliasesResult.status === "rejected") console.log("Could not fetch model aliases");
  if (disabledResult.status === "rejected") console.log("Could not fetch disabled models");

  const connections = connectionsResult.status === "fulfilled"
    ? connectionsResult.value.filter((connection) => connection.isActive !== false)
    : [];
  const combos = combosResult.status === "fulfilled" ? combosResult.value : [];
  const customModels = customModelsResult.status === "fulfilled" ? customModelsResult.value : [];
  const modelAliases = aliasesResult.status === "fulfilled" ? aliasesResult.value : {};
  const disabledByAlias = disabledResult.status === "fulfilled" ? disabledResult.value : {};
  const isDisabled = (alias, modelId) => Array.isArray(disabledByAlias[alias]) && disabledByAlias[alias].includes(modelId);

  const activeConnectionByProvider = new Map();
  for (const conn of connections) {
    if (!activeConnectionByProvider.has(conn.provider)) {
      activeConnectionByProvider.set(conn.provider, conn);
    }
  }

  const models = [];

  // Lookup map so aggregateComboCapabilities can recursively resolve nested combos
  const comboByName = Object.fromEntries(combos.map((c) => [c.name, c.models]));

  // Combos first (filtered by kind). Web combos expose `kind` so AI knows search vs fetch.
  for (const combo of combos) {
    if (!comboMatchesKinds(combo, kindFilter)) continue;
    const entry = {
      id: combo.name,
      object: "model",
      owned_by: "combo",
    };
    if (combo.kind === "webSearch" || combo.kind === "webFetch") {
      entry.kind = combo.kind;
    } else {
      const comboCaps = aggregateComboCapabilities(combo.models, comboByName);
      if (comboCaps) entry.capabilities = comboCaps;
    }
    models.push(entry);
  }

  if (providerReadFailed) {
    // DB unavailable -> return static models, filtered by per-model kind.
    // Empty configured providers is not an error and must not expose unconfigured models.
    const aliasToProviderId = Object.fromEntries(
      Object.entries(PROVIDER_ID_TO_ALIAS).map(([id, alias]) => [alias, id])
    );
    for (const [alias, providerModels] of Object.entries(PROVIDER_MODELS)) {
      const providerId = aliasToProviderId[alias] || alias;
      if (!providerMatchesKinds(providerId, kindFilter)) continue;
      if (!shouldExposeStaticProvider(providerId)) continue;
      for (const model of providerModels) {
        if (!kindFilter.includes(modelKind(model))) continue;
        if (isDisabled(alias, model.id)) continue;
        models.push({
          id: `${alias}/${model.id}`,
          object: "model",
          owned_by: alias,
          capabilities: getCapabilitiesForModel(alias, model.id),
        });
      }
    }

  } else {
    const providerEntries = await mapWithConcurrency(
      Array.from(activeConnectionByProvider.entries()),
      async ([providerId, conn], _index, signal) => {
        const providerOutput = [];
        if (!providerMatchesKinds(providerId, kindFilter, conn)) return providerOutput;

        const staticAlias = PROVIDER_ID_TO_ALIAS[providerId] || providerId;
        const outputAlias = (
        conn?.providerSpecificData?.prefix
        || getProviderAlias(providerId)
        || staticAlias
        ).trim();
        const providerModels = PROVIDER_MODELS[staticAlias] || [];
      // Build kind lookup for static models so we can filter even when only IDs are exposed
      const staticModelKindById = new Map(
        providerModels.map((m) => [m.id, modelKind(m)])
      );
      let liveModelKindById = new Map();
      let liveCapabilitiesById = new Map();

      // Single source of truth for provider model lists — shared with the
      // manual refresh endpoint so a refreshed cache yields exactly the list
      // the live path would have produced.
      const resolved = await resolveProviderModelList(conn, providerId, { allowLive, signal });
      const rawModelIds = resolved.models.map((m) => m.id);
      liveModelKindById = new Map(
        resolved.models.filter((m) => m?.kind).map((m) => [m.id, modelKind(m)])
      );
      liveCapabilitiesById = new Map(
        resolved.models.filter((m) => m?.capabilities).map((m) => [m.id, m.capabilities])
      );

      const modelIds = rawModelIds
        .map((modelId) => stripProviderPrefix(modelId, [outputAlias, staticAlias, providerId]))
        .filter((modelId) => typeof modelId === "string" && modelId.trim() !== "");

      const customModelKindById = new Map();
      const customModelIds = customModels
        .filter((m) => {
          if (!m?.id) return false;
          const kind = getModelKind(m) || LLM_KIND;
          // imageToText custom models are vision-capable chat models: expose them
          // both in the default LLM list and in /v1/models/image-to-text.
          if (!kindFilter.includes(kind) && !(kind === "imageToText" && kindFilter.includes(LLM_KIND))) return false;
          const alias = m.providerAlias;
          return alias === staticAlias || alias === outputAlias || alias === providerId;
        })
        .map((m) => {
          const modelId = stripProviderPrefix(m.id, [outputAlias, staticAlias, providerId]);
          if (modelId) customModelKindById.set(modelId, getModelKind(m) || LLM_KIND);
          return modelId;
        })
        .filter((modelId) => modelId !== "");

      // Build reverse map: fullModelId → aliasKey (for /v1/models to show alias as id)
      const fullModelToAlias = {};
      for (const [aliasKey, fullModel] of Object.entries(modelAliases || {})) {
        if (typeof fullModel === "string") {
          // Normalize: match on any of outputAlias/staticAlias/providerId prefix
          const normalized = fullModel.startsWith(`${outputAlias}/`) ? fullModel
            : fullModel.startsWith(`${staticAlias}/`) ? `${outputAlias}/${fullModel.slice(staticAlias.length + 1)}`
            : fullModel.startsWith(`${providerId}/`) ? `${outputAlias}/${fullModel.slice(providerId.length + 1)}`
            : fullModel;
          fullModelToAlias[normalized] = aliasKey;
        }
      }

      const mergedModelIds = Array.from(new Set([...modelIds, ...customModelIds]));

      for (const modelId of mergedModelIds) {
        // Resolve kind: prefer custom/live metadata, then static, then ID heuristics.
        const customKind = customModelKindById.get(modelId);
        const liveKind = liveModelKindById.get(modelId);
        const kind = conn?.providerSpecificData?.mediaKind
          || customKind
          || liveKind
          || staticModelKindById.get(modelId)
          || inferKindFromUnknownModelId(modelId);
        // imageToText custom models stay in the LLM list (vision-capable chat models)
        const allowAsLlm = kind === "imageToText" && kindFilter.includes(LLM_KIND);
        if (!kindFilter.includes(kind) && !allowAsLlm) continue;
        if (isDisabled(outputAlias, modelId) || isDisabled(staticAlias, modelId)) continue;

        const fullModelId = `${outputAlias}/${modelId}`;
        const aliasKey = fullModelToAlias[fullModelId];

        // Live-catalog resolvers (kiro/qoder/github/clinepass) mostly only return
        // { id, name } — no per-model capability data. Fall back to the same
        // pattern-matched capabilities the dashboard uses (useModelCaps.js) so
        // dynamically-discovered LLM models still surface vision/reasoning/search/tools.
        const liveCaps = liveCapabilitiesById.get(modelId);
        const serviceCaps = capabilitiesFromServiceKind(customKind || liveKind);
        const caps = liveCaps || serviceCaps || (kind === LLM_KIND ? getCapabilitiesForModel(providerId, modelId) : null);

        // Always emit the original model entry
        const originalModel = {
          id: fullModelId,
          object: "model",
          owned_by: outputAlias,
        };
        if (caps) originalModel.capabilities = caps;
        // Token limits under the snake_case names the OpenAI/OpenRouter
        // convention uses. `capabilities.contextWindow` is camelCase and nested,
        // so clients matching context_length find nothing, fall back to guessing
        // the window from the model name, and guess high — a 372k model read as
        // 1.05M never reaches its compaction threshold and hard-fails upstream.
        // Emitted at top level because not every client recurses into nested
        // objects; the camelCase `capabilities` block stays for compatibility.
        if (kind === LLM_KIND || allowAsLlm) {
          let contextWindow = caps?.contextWindow;
          let maxOutput = caps?.maxOutput;
          // Live-catalog and service-kind capabilities are usually partial
          // (often just { tools: true }), so fill the gaps from the static
          // table rather than emitting null and leaving clients to guess.
          if (!Number.isFinite(contextWindow) || !Number.isFinite(maxOutput)) {
            const fallback = getCapabilitiesForModel(providerId, modelId);
            if (!Number.isFinite(contextWindow)) contextWindow = fallback.contextWindow;
            if (!Number.isFinite(maxOutput)) maxOutput = fallback.maxOutput;
          }
          if (Number.isFinite(contextWindow)) originalModel.context_length = contextWindow;
          if (Number.isFinite(maxOutput)) originalModel.max_completion_tokens = maxOutput;
        }
        providerOutput.push(originalModel);

        // Also emit an alias entry (so clients can use the short alias)
        if (aliasKey) {
          const aliasModel = {
            id: aliasKey,
            object: "model",
            owned_by: outputAlias,
          };
          if (caps) aliasModel.capabilities = caps;
          if (Number.isFinite(originalModel.context_length)) aliasModel.context_length = originalModel.context_length;
          if (Number.isFinite(originalModel.max_completion_tokens)) aliasModel.max_completion_tokens = originalModel.max_completion_tokens;
          providerOutput.push(aliasModel);
        }
      }

      // Web search/fetch — provider IS the model, expose as {alias}/search and/or {alias}/fetch with explicit kind
      const providerInfo = AI_PROVIDERS[providerId];
      if (kindFilter.includes("webSearch") && providerInfo?.searchConfig) {
        providerOutput.push({
          id: `${outputAlias}/search`,
          object: "model",
          kind: "webSearch",
          owned_by: outputAlias,
        });
      }
      if (kindFilter.includes("webFetch") && providerInfo?.fetchConfig) {
        providerOutput.push({
          id: `${outputAlias}/fetch`,
          object: "model",
          kind: "webFetch",
          owned_by: outputAlias,
        });
      }
        return providerOutput;
      },
      {
        concurrency: providerConcurrency,
        timeoutMs: providerTimeoutMs,
        onError: (error, [providerId]) => {
          console.log(`Model aggregation failed for ${providerId}: ${error?.message || error}`);
          return [];
        },
      },
    );
    models.push(...providerEntries.flat());
  }

  const dedupedModels = [];
  const seenModelIds = new Set();
  for (const model of models) {
    if (!model?.id || seenModelIds.has(model.id)) continue;
    seenModelIds.add(model.id);
    dedupedModels.push(model);
  }

  return dedupedModels;
}

/**
 * Handle CORS preflight
 */
export async function OPTIONS() {
  return new Response(null, {
    headers: {
      "Access-Control-Allow-Origin": "*",
      "Access-Control-Allow-Methods": "GET, OPTIONS",
      "Access-Control-Allow-Headers": "*",
    },
  });
}

/**
 * GET /v1/models - OpenAI compatible models list (LLM/chat models only by default).
 * For other capabilities use /v1/models/{kind} (image, tts, stt, embedding, image-to-text, web).
 */
export async function GET(request) {
  try {
    // Detect cross-instance recursive /models fetch (another 9router fetching our /models)
    const skipDynamicFetch = request?.headers?.get(INTERNAL_MODELS_FETCH_HEADER) === "1";
    const now = Date.now();

    if (!skipDynamicFetch && modelsListCache.body && now - modelsListCache.ts < MODELS_LIST_TTL_MS) {
      return new Response(modelsListCache.body, {
        headers: modelsListHeaders({ "x-models-cache": "memory" }),
      });
    }

    // Cross-isolate edge cache (Workers): avoids re-paying CPU on every cold isolate.
    if (!skipDynamicFetch) {
      const edgeBody = await readModelsEdgeCache(request);
      if (edgeBody) {
        modelsListCache.body = edgeBody;
        modelsListCache.ts = Date.now();
        return new Response(edgeBody, {
          headers: modelsListHeaders({ "x-models-cache": "edge" }),
        });
      }
    }

    // Coalesce concurrent rebuilds so a burst of /v1/models only pays CPU once.
    if (!skipDynamicFetch && modelsListCache.inflight) {
      const body = await modelsListCache.inflight;
      return new Response(body, {
        headers: modelsListHeaders({ "x-models-cache": "coalesced" }),
      });
    }

    const build = (async () => {
      const data = await buildModelsList([LLM_KIND], { skipDynamicFetch });
      return JSON.stringify({ object: "list", data });
    })();

    if (!skipDynamicFetch) modelsListCache.inflight = build;

    let body;
    try {
      body = await build;
    } finally {
      if (modelsListCache.inflight === build) modelsListCache.inflight = null;
    }

    if (!skipDynamicFetch && body) {
      modelsListCache.body = body;
      modelsListCache.ts = Date.now();
      await writeModelsEdgeCache(request, body);
    }

    return new Response(body, {
      headers: modelsListHeaders({ "x-models-cache": "miss" }),
    });
  } catch (error) {
    console.log("Error fetching models:", error);
    // Stale cache is better than a hard failure when a rebuild throws.
    if (modelsListCache.body) {
      return new Response(modelsListCache.body, {
        headers: modelsListHeaders({ "x-models-cache": "stale-memory" }),
      });
    }
    const edgeBody = await readModelsEdgeCache(request).catch(() => null);
    if (edgeBody) {
      return new Response(edgeBody, {
        headers: modelsListHeaders({ "x-models-cache": "stale-edge" }),
      });
    }
    return Response.json(
      { error: { message: error.message, type: "server_error" } },
      { status: 500 }
    );
  }
}
