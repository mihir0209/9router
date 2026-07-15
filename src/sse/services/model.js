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
      // Helper: strip only the local prefix, keep the upstream provider path.
      // e.g. prefix="9", id="9/v/posiden", modelStr="9/v/posiden/deepseek-v4-flash"
      // → returns "v/posiden/deepseek-v4-flash" (sent to upstream as-is)
      const stripLocalPrefix = (node, modelStr, fallbackModel) => {
        const prefixSlash = node.prefix + "/";
        if (modelStr.startsWith(prefixSlash)) {
          return modelStr.slice(prefixSlash.length);
        }
        return fallbackModel;
      };

      const allNodes = await getProviderNodes();
      const matchedNode = allNodes.find((node) => node.prefix === parsed.providerAlias);
      if (matchedNode) {
        const upstreamModel = stripLocalPrefix(matchedNode, modelStr, parsed.model);
        return { provider: matchedNode.id, model: upstreamModel };
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
  
  // After alias resolution, check if the resolved provider is a prefix for a provider node.
  // This handles cases like alias resolving to "9/v/posiden/deepseek-v4-flash"
  // where "9" is a provider node prefix, not the actual provider ID.
  if (result && result.provider) {
    const allNodes = await getProviderNodes();
    const matchedNode = allNodes.find((node) => node.prefix === result.provider);
    if (matchedNode) {
      // Reconstruct the full model string from the alias resolution result,
      // then strip only the local prefix to get the upstream model.
      const resolvedFullModel = result.provider + "/" + result.model;
      const prefixSlash = matchedNode.prefix + "/";
      const upstreamModel = resolvedFullModel.startsWith(prefixSlash) 
        ? resolvedFullModel.slice(prefixSlash.length) 
        : result.model;
      return { provider: matchedNode.id, model: upstreamModel };
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
