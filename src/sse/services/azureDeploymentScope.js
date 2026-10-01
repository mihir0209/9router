import { azureDeploymentMatches } from "../../../open-sse/executors/azure.js";

/**
 * Scope account selection for Azure to connections that can actually serve the
 * requested model.
 *
 * Azure resolves every request to a per-connection deployment, and the URL path
 * is what upstream honours. So an unscoped pick among connections pinned to
 * *different* deployments can return a model the client never asked for — a
 * `azure/gpt-5.6-terra` request served by a connection pinned to `gpt-5.6-sol`.
 *
 * This narrows the candidate set to matching connections so every existing
 * selection strategy (fill-first, round-robin, adaptive) then operates on
 * connections that can genuinely answer. When nothing matches, the full set is
 * returned unchanged: an unmatched model is still worth attempting, because the
 * previous behaviour was to route it somewhere and a hard failure would be a
 * regression for connections that rely on a single pinned deployment.
 *
 * Non-Azure providers are returned untouched.
 */
export function scopeConnectionsByDeployment(connections, providerId, model) {
  if (!Array.isArray(connections) || connections.length < 2) return connections;
  if (String(providerId || "").toLowerCase() !== "azure") return connections;
  if (!model) return connections;

  const matching = connections.filter((c) => azureDeploymentMatches(c, model));
  if (matching.length === 0) return connections;
  if (matching.length === connections.length) return connections;

  return matching;
}