// Re-export from open-sse with localDb integration
import { getModelAliases, getComboByName, getProviderNodes, getModelDisplayNames } from "@/lib/localDb";
import { parseModel as parseModelCore, resolveModelAliasFromMap, getModelInfoCore } from "open-sse/services/model.js";
import REGISTRY from "open-sse/providers/registry/index.js";

// Local provider alias overrides (HMR-friendly, applied on top of open-sse map)
const LOCAL_PROVIDER_ALIASES = {
  xmtp: "xiaomi-tokenplan",
  "xiaomi-tokenplan": "xiaomi-tokenplan",
};

const RESERVED_PROVIDER_PREFIXES = new Set(Object.keys(LOCAL_PROVIDER_ALIASES));
for (const entry of REGISTRY) {
  RESERVED_PROVIDER_PREFIXES.add(entry.id);
  if (entry.alias) RESERVED_PROVIDER_PREFIXES.add(entry.alias);
  for (const alias of entry.aliases || []) RESERVED_PROVIDER_PREFIXES.add(alias);
}

export function parseModel(modelStr) {
  const parsed = parseModelCore(modelStr);
  if (parsed?.providerAlias && LOCAL_PROVIDER_ALIASES[parsed.providerAlias]) {
    return { ...parsed, provider: LOCAL_PROVIDER_ALIASES[parsed.providerAlias] };
  }
  return parsed;
}

/**
 * Resolve model alias from localDb
 */
export async function resolveModelAlias(alias) {
  const aliases = await getModelAliases();
  return resolveModelAliasFromMap(alias, aliases);
}

/**
 * Get full model info (parse or resolve)
 */
export async function getModelInfo(modelStr) {
  if (!modelStr || typeof modelStr !== "string") {
    return { provider: null, model: null };
  }

  // First, check if this is a display name and resolve it
  try {
    const displayNames = await getModelDisplayNames();
    const reverseDisplayNames = {};
    for (const [fullId, displayName] of Object.entries(displayNames)) {
      if (displayName && fullId) {
        reverseDisplayNames[displayName] = fullId;
      }
    }
    if (reverseDisplayNames[modelStr]) {
      modelStr = reverseDisplayNames[modelStr];
    }
  } catch (e) {
    // Ignore display name lookup errors
  }

  const parsed = parseModel(modelStr);

  if (!parsed.isAlias) {
    // Provider-node prefixes are user-defined. They must not override built-in
    // provider ids/aliases such as `cf`, `cloudflare-ai`, `openai`, or `hf`.
    if (!RESERVED_PROVIDER_PREFIXES.has(parsed.providerAlias)) {
      const openaiNodes = await getProviderNodes({ type: "openai-compatible" });
      const matchedOpenAI = openaiNodes.find((node) => node.prefix === parsed.providerAlias);
      if (matchedOpenAI) {
        // Extract model name by removing provider ID prefix from model string
        // Model string format: prefix/providerId/modelName → modelName
        const providerIdPrefix = matchedOpenAI.id + "/";
        const modelStrAfterPrefix = modelStr.startsWith(providerIdPrefix) 
          ? modelStr.slice(providerIdPrefix.length) 
          : parsed.model;
        return { provider: matchedOpenAI.id, model: modelStrAfterPrefix };
      }

      const anthropicNodes = await getProviderNodes({ type: "anthropic-compatible" });
      const matchedAnthropic = anthropicNodes.find((node) => node.prefix === parsed.providerAlias);
      if (matchedAnthropic) {
        const providerIdPrefix = matchedAnthropic.id + "/";
        const modelStrAfterPrefix = modelStr.startsWith(providerIdPrefix) 
          ? modelStr.slice(providerIdPrefix.length) 
          : parsed.model;
        return { provider: matchedAnthropic.id, model: modelStrAfterPrefix };
      }

      const embeddingNodes = await getProviderNodes({ type: "custom-embedding" });
      const matchedEmbedding = embeddingNodes.find((node) => node.prefix === parsed.providerAlias);
      if (matchedEmbedding) {
        const providerIdPrefix = matchedEmbedding.id + "/";
        const modelStrAfterPrefix = modelStr.startsWith(providerIdPrefix) 
          ? modelStr.slice(providerIdPrefix.length) 
          : parsed.model;
        return { provider: matchedEmbedding.id, model: modelStrAfterPrefix };
      }
    }
    return {
      provider: parsed.provider,
      model: parsed.model
    };
  }

  // Check if this is a combo name before resolving as alias
  // This prevents combo names from being incorrectly routed to providers
  const combo = await getComboByName(parsed.model);
  if (combo) {
    // Return null provider to signal this should be handled as combo
    // The caller (handleChat) will detect this and handle it as combo
    return { provider: null, model: parsed.model };
  }

  const result = await getModelInfoCore(modelStr, getModelAliases);
  
  // After alias resolution, check if the resolved provider is a prefix for a provider node
  // This handles cases like alias resolving to "9/v/posiden/deepseek-v4-flash"
  if (result && result.provider) {
    const openaiNodes = await getProviderNodes({ type: "openai-compatible" });
    const matchedOpenAI = openaiNodes.find((node) => node.prefix === result.provider);
    if (matchedOpenAI) {
      // Extract model name by removing provider ID prefix from resolved model string
      const resolvedFullModel = result.provider + "/" + result.model;
      const providerIdPrefix = matchedOpenAI.id + "/";
      const modelStrAfterPrefix = resolvedFullModel.startsWith(providerIdPrefix) 
        ? resolvedFullModel.slice(providerIdPrefix.length) 
        : result.model;
      return { provider: matchedOpenAI.id, model: modelStrAfterPrefix };
    }

    const anthropicNodes = await getProviderNodes({ type: "anthropic-compatible" });
    const matchedAnthropic = anthropicNodes.find((node) => node.prefix === result.provider);
    if (matchedAnthropic) {
      const resolvedFullModel = result.provider + "/" + result.model;
      const providerIdPrefix = matchedAnthropic.id + "/";
      const modelStrAfterPrefix = resolvedFullModel.startsWith(providerIdPrefix) 
        ? resolvedFullModel.slice(providerIdPrefix.length) 
        : result.model;
      return { provider: matchedAnthropic.id, model: modelStrAfterPrefix };
    }

    const embeddingNodes = await getProviderNodes({ type: "custom-embedding" });
    const matchedEmbedding = embeddingNodes.find((node) => node.prefix === result.provider);
    if (matchedEmbedding) {
      const resolvedFullModel = result.provider + "/" + result.model;
      const providerIdPrefix = matchedEmbedding.id + "/";
      const modelStrAfterPrefix = resolvedFullModel.startsWith(providerIdPrefix) 
        ? resolvedFullModel.slice(providerIdPrefix.length) 
        : result.model;
      return { provider: matchedEmbedding.id, model: modelStrAfterPrefix };
    }
  }
  
  return result;
}

/**
 * Check if model is a combo and get models list
 * @returns {Promise<string[]|null>} Array of models or null if not a combo
 */
export async function getComboModels(modelStr) {
  // Only check if it's not in provider/model format
  if (modelStr.includes("/")) return null;

  const combo = await getComboByName(modelStr);
  if (combo && combo.models && combo.models.length > 0) {
    return combo.models;
  }
  return null;
}
