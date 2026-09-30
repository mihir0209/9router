import { PROVIDER_MODELS, PROVIDER_ID_TO_ALIAS, getModelKind } from "@/shared/constants/models";
import {
  AI_PROVIDERS,
  getProviderAlias,
  isAnthropicCompatibleProvider,
  isOpenAICompatibleProvider,
} from "@/shared/constants/providers";
import { getProviderConnections, getCombos, getCustomModels, getModelAliases, getApiKeyByKey } from "@/lib/localDb";
import { getDisabledModels } from "@/lib/disabledModelsDb";
import { capabilitiesFromServiceKind, getCapabilitiesForModel, aggregateComboCapabilities } from "open-sse/providers/capabilities.js";
import { mapWithConcurrency } from "@/lib/async/mapWithConcurrency.js";
import { resolveProviderModelList, INTERNAL_MODELS_FETCH_HEADER } from "@/lib/modelCatalog/providerModelResolver.js";
import { getSettings } from "@/lib/localDb";
import { normalizeModelsListSource } from "@/lib/db/repos/settingsRepo.js";
import {
  authenticateModelsRequest,
  modelsCacheId,
  modelsListHeaders,
  parseModelList,
  unauthorizedModelsResponse,
  withModelsCache,
} from "@/lib/modelCatalog/modelsListCache.js";


// LLM kind sentinel — combos/models with no explicit kind default to LLM
const LLM_KIND = "llm";
const CLOUDFLARE_STATIC_PROVIDER_BLOCKLIST = new Set(["opencode"]);
// Live provider fan-out is never allowed on Workers: it cannot complete inside
// the CPU budget, so `live` degrades to the cached catalogue there.
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
const MODEL_PROVIDER_CONCURRENCY = 4;
const MODEL_PROVIDER_TIMEOUT_MS = 6000;

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

  // An allow-list on the caller's key narrows the list. Empty or "*" means
  // unrestricted, and then the full catalog is correct.
  const rawAllowed = options.allowedModels;
  const allowedList = typeof rawAllowed === "string"
    ? rawAllowed.split(",").map((s) => s.trim()).filter(Boolean)
    : Array.isArray(rawAllowed) ? rawAllowed.map((s) => String(s).trim()).filter(Boolean) : [];
  const restrictedModels = allowedList.length > 0 && !allowedList.includes("*") ? allowedList : null;
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

  // ── Restricted API key: answer from the database, never from providers ──
  //
  // A key with an allow-list is entitled to a known, small set of models. That
  // set is already in hand, so listing it must not cost a round trip to every
  // configured provider to discover models this caller could not use anyway.
  // This branch returns before the provider-connection loop entirely, which is
  // the point: the full list fans out to 10+ upstreams, and a restricted key
  // would pay that cost for a result it would then be forbidden to use.
  //
  // An allow-listed id is emitted even when the static catalog has never heard
  // of it. That is deliberate in both directions:
  //
  //   Hiding it would be wrong when the model is real but dynamically
  //   discovered upstream. The admin allow-listed it, enforcement permits it, and
  //   the client would be unable to see something it is allowed to call.
  //
  //   Not hiding it can show an entry with no capability metadata, when the
  //   allow-list names something that does not exist. The cost of that is a
  //   client seeing a model it cannot usefully call; the cost of hiding is a
  //   client unable to see one it can. The admin made the claim, so the claim
  //   is surfaced.
  if (restrictedModels) {
    const emitted = new Set();
    // "providerAlias/modelId" -> service kind, from the static catalogue. Built
    // here rather than globally so the restricted path stays independent of how
    // the main loop is written.
    const staticKindByFullId = new Map();
    for (const [aliasKey, providerModels] of Object.entries(PROVIDER_MODELS)) {
      for (const staticModel of providerModels) {
        if (staticModel?.id) staticKindByFullId.set(`${aliasKey}/${staticModel.id}`, modelKind(staticModel));
      }
    }
    const push = (entry) => {
      if (!entry?.id || emitted.has(entry.id)) return;
      emitted.add(entry.id);
      models.push(entry);
    };

    // Flatten each allowed entry. A combo is authorised by name (see
    // enforceApiKeyRestrictions), so listing one means listing what it can
    // actually reach — a client that can call `auto` should see its members.
      const wanted = [];
      const seenCombo = new Set();
      const expandCombo = (name) => {
        if (seenCombo.has(name)) return;   // bounded: a cyclic combo stops here
        seenCombo.add(name);
        const members = comboByName[name];
        if (!Array.isArray(members)) return;
        for (const member of members) {
          if (comboByName[member]) expandCombo(member);
          else wanted.push(member);
        }
      };
      for (const entry of restrictedModels) {
        // A combo is only in scope for the kind being asked about. Without this
        // /v1/models/image answered a restricted key with its chat models: the
        // branch built the allow-list expansion and returned, never consulting
        // kindFilter, so every capability route returned the same list.
        const combo = combos.find((c) => c.name === entry);
        if (combo && !comboMatchesKinds(combo, kindFilter)) continue;
        if (comboByName[entry]) {
          expandCombo(entry);
        } else {
          wanted.push(entry);
        }
      }

    // alias -> full id, so an allow-list entry written as an alias still resolves
    const aliasToFull = {};
    for (const [aliasKey, fullModel] of Object.entries(modelAliases || {})) {
      if (typeof fullModel === "string") aliasToFull[aliasKey] = fullModel;
    }

    for (const rawId of wanted) {
      if (typeof rawId !== "string" || rawId.trim() === "") continue;
      const fullId = aliasToFull[rawId] || rawId;
      const slash = fullId.indexOf("/");
      const alias = slash > 0 ? fullId.slice(0, slash) : rawId;
      const modelId = slash > 0 ? fullId.slice(slash + 1) : rawId;
      if (isDisabled(alias, modelId)) continue;

        const caps = getCapabilitiesForModel(alias, modelId);
        // Same kind test the unrestricted path applies to static models, so a
        // restricted key's /v1/models/image lists image models and nothing else.
        // The kind comes from the static catalogue entry, not from capabilities:
        // capabilities describe features (reasoning, vision, context window) and
        // do not say whether a model is an image or a chat model.
        const kind = staticKindByFullId.get(fullId) || inferKindFromUnknownModelId(fullId);
        const inScope = kindFilter.includes(kind)
          || (kind === "imageToText" && kindFilter.includes(LLM_KIND));
        if (!inScope) continue;
        const entry = { id: fullId, object: "model", owned_by: alias };
      if (caps) entry.capabilities = caps;
      if (Number.isFinite(caps?.contextWindow)) entry.context_length = caps.contextWindow;
      if (Number.isFinite(caps?.maxOutput)) entry.max_completion_tokens = caps.maxOutput;
      push(entry);
      // The alias is a usable id in its own right, so expose it too — that is
      // what an allow-list entry usually looks like.
      if (aliasToFull[rawId]) push({ ...entry, id: rawId });
    }

    // A custom model the allow-list names by id, even if it is not in the static
    // catalog. Otherwise restricting a key to a custom model returns nothing.
    for (const m of customModels) {
      if (!m?.id) continue;
      const fullId = `${m.providerAlias}/${m.id}`;
      if (!emitted.has(fullId) && (emitted.has(m.id) || wanted.includes(fullId) || wanted.includes(m.id))) {
        const caps = getCapabilitiesForModel(m.providerAlias, m.id);
        push({ id: fullId, object: "model", owned_by: m.providerAlias, capabilities: caps || undefined });
      }
    }

    return models;
  }

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

    const auth = await authenticateModelsRequest(request);
    if (!auth.ok) {
      return unauthorizedModelsResponse(auth.error, auth.status);
    }
    const keyRecord = auth.keyRecord;
    const allowedModels = parseModelList(keyRecord.allowedModels);

    const { body, source } = await withModelsCache({
      request,
      cacheId: modelsCacheId(keyRecord),
      skipCache: skipDynamicFetch,
      build: async () => {
        const data = await buildModelsList([LLM_KIND], { skipDynamicFetch, allowedModels });
        return { object: "list", data };
      },
    });

    return new Response(body, {
      headers: modelsListHeaders({ "x-models-cache": source }),
    });
  } catch (error) {
    console.log("Error fetching models:", error);
    return Response.json(
      { error: { message: error.message, type: "server_error" } },
      { status: 500, headers: modelsListHeaders() }
    );
  }
}
