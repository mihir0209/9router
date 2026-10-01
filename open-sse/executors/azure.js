import { DefaultExecutor } from "./default.js";

/**
 * Normalise a connection's configured model pool into bare model ids.
 * Accepts both `models` (media pool) and `enabledModels` (allow-list), matching
 * what `src/lib/modelCatalog/providerModelResolver.js` treats as the advertised
 * pool, and tolerates bare ids or `provider/`-prefixed ids.
 */
export function normalizeAzureModelPool(providerSpecificData) {
  const configured = providerSpecificData?.models ?? providerSpecificData?.enabledModels;
  if (!Array.isArray(configured)) return [];
  return configured
    .filter((id) => typeof id === "string" && id.trim())
    .map((id) => id.trim().replace(/^azure\//i, ""));
}

/**
 * Resolve which Azure deployment this request must actually hit.
 *
 * Azure's `/deployments/{d}/chat/completions` route is keyed off the URL path, so
 * the `model` field in the request body is ignored upstream. The deployment is
 * therefore the only thing that decides which model answers.
 *
 * Historically the stored `deployment` unconditionally won, because it sat first
 * in an `||` chain, and the configured pool was display-only. That let account
 * selection hand a client a model it never asked for: with several connections
 * pinned to different deployments, any of them would serve any request.
 *
 * Precedence now:
 *   1. the requested model, but only when the connection explicitly lists it in
 *      its pool — the admin has said this deployment serves it;
 *   2. the connection's stored `deployment`;
 *   3. the requested model (an unpooled connection has nothing else to go on);
 *   4. `AZURE_DEPLOYMENT`, then `"gpt-4"`.
 *
 * Step 3 before step 4 keeps `AZURE_DEPLOYMENT` meaningful only when there is no
 * model to route to. Connections with no pool configured are unaffected, which
 * preserves existing single-deployment setups.
 */
export function resolveAzureDeployment(providerSpecificData, model) {
  const requested = typeof model === "string" ? model.trim().replace(/^azure\//i, "") : "";
  const pool = normalizeAzureModelPool(providerSpecificData);

  if (requested && pool.includes(requested)) return requested;

  return providerSpecificData?.deployment
    || requested
    || process.env.AZURE_DEPLOYMENT
    || "gpt-4";
}

/**
 * Whether this connection can serve the requested model without substituting a
 * different one. Used to scope account selection so a client is not silently
 * answered by an unrelated pinned deployment.
 */
export function azureDeploymentMatches(connection, model) {
  const requested = typeof model === "string" ? model.trim().replace(/^azure\//i, "") : "";
  if (!requested) return false;

  const psd = connection?.providerSpecificData || {};
  const pool = normalizeAzureModelPool(psd);
  if (pool.includes(requested)) return true;

  const deployment = psd.deployment;
  if (!deployment) return false;

  // An unpinned connection has no deployment to contradict the request.
  const normalized = String(deployment).trim();
  return normalized === requested || normalized.toLowerCase() === requested.toLowerCase();
}

export class AzureExecutor extends DefaultExecutor {
  constructor() {
    super("azure");
  }

  buildUrl(model, stream, urlIndex = 0, credentials = null) {
    const azureEndpoint = credentials?.providerSpecificData?.azureEndpoint
      || process.env.AZURE_ENDPOINT
      || "https://api.openai.com";

    const apiVersion = credentials?.providerSpecificData?.apiVersion
      || process.env.AZURE_API_VERSION
      || "2024-10-01-preview";

    const deployment = resolveAzureDeployment(credentials?.providerSpecificData, model);

    const endpoint = azureEndpoint.replace(/\/$/, "");
    return `${endpoint}/openai/deployments/${deployment}/chat/completions?api-version=${apiVersion}`;
  }

  buildHeaders(credentials, stream = true) {
    const headers = {
      "Content-Type": "application/json",
      ...this.config.headers
    };

    const apiKey = credentials?.apiKey
      || credentials?.accessToken
      || process.env.OPENAI_API_KEY;

    if (apiKey) {
      headers["api-key"] = apiKey;
    }

    const organization = credentials?.providerSpecificData?.organization
      || process.env.AZURE_ORGANIZATION;

    if (organization) {
      headers["OpenAI-Organization"] = organization;
    }

    if (stream) {
      headers["Accept"] = "text/event-stream";
    }

    return headers;
  }

  transformRequest(model, body, stream, credentials) {
    return super.transformRequest(model, body, stream, credentials);
  }
}
