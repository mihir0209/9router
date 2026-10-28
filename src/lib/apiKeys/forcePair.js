// Pair-consistency check for forceProvider + forceModel. At request time the
// enforcement layer resolves the pinned model (alias, display name, provider
// node prefix, combo expansion) and answers 403 unless every resolved leaf
// belongs to forceProvider. Run the same resolution at write time so a pair
// that could never succeed is rejected up front instead of failing every call.
import { resolveRequestedModels } from "@/sse/services/apiKeyEnforcement.js";

/**
 * @returns {Promise<string|null>} an error message, or null when the pair is
 *   consistent (or only one of the two pins is set).
 */
export async function findForcePairMismatch({ forceProvider, forceModel }, resolve = resolveRequestedModels) {
  const provider = String(forceProvider || "").trim();
  const model = String(forceModel || "").trim();
  if (!provider || !model) return null;
  const resolved = await resolve(model, null);
  const providers = resolved?.providers || [];
  if (!providers.length) return `forceModel '${model}' does not resolve to a provider, so it cannot satisfy forceProvider '${provider}'`;
  if (providers.some((entry) => entry !== provider)) {
    return `forceModel '${model}' routes to provider '${providers.join(", ")}', which does not match forceProvider '${provider}'`;
  }
  return null;
}
