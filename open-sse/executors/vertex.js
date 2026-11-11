import { BaseExecutor } from "./base.js";
import { PROVIDERS } from "../config/providers.js";
import { getModelMeta } from "../config/providerModels.js";
import { parseVertexSaJson, refreshVertexToken, refreshGoogleToken } from "../services/tokenRefresh.js";
import { proxyAwareFetch } from "../utils/proxyFetch.js";

// Cache project IDs resolved from raw API keys { apiKey → projectId }
const projectIdCache = new Map();

/**
 * Parse Google ADC user credential JSON from apiKey string.
 * This is the format produced by `gcloud auth application-default login`.
 */
function parseVertexAdcJson(apiKey) {
  if (typeof apiKey !== "string") return null;
  try {
    const parsed = JSON.parse(apiKey);
    if (
      parsed.type === "authorized_user" &&
      parsed.client_id &&
      parsed.client_secret &&
      parsed.refresh_token
    ) {
      return parsed;
    }
    return null;
  } catch {
    return null;
  }
}

/**
 * Resolve GCP project ID from a raw Vertex API key.
 * Sends a dummy 404 request and parses "projects/{id}" from the error message.
 */
async function resolveProjectId(apiKey) {
  if (projectIdCache.has(apiKey)) return projectIdCache.get(apiKey);

  const res = await fetch(
    `https://aiplatform.googleapis.com/v1/publishers/google/models/__probe__:generateContent?key=${apiKey}`,
    { method: "POST", headers: { "Content-Type": "application/json" }, body: "{}" }
  );
  const json = await res.json().catch(() => null);
  const msg = json?.[0]?.error?.message || json?.error?.message || "";
  const match = msg.match(/projects\/([^/]+)\//);
  const projectId = match?.[1] || null;

  if (projectId) projectIdCache.set(apiKey, projectId);
  return projectId;
}

/**
 * Resolves the location to use for Vertex AI.
 *
 * Defaults to "global" for all models unless explicitly overridden.
 * Allows custom settings per-model (providerSpecificData.modelLocations) or
 * connection-level custom settings (providerSpecificData.location).
 * If a custom regional location is specified on the connection (e.g. asia-southeast1),
 * models like Carbon that only exist on global/us/eu will safely route to global unless
 * a per-model override explicitly targets another supported region.
 */
export function resolveVertexLocation(provider, model, configuredLocation, providerSpecificData = null) {
  const baseModel = (typeof model === "string" ? model : "").replace(/\([^()]+\)\s*$/, "").trim();

  // 1. Custom per-model override from providerSpecificData (user custom setting)
  const perModelOverride = providerSpecificData?.modelLocations?.[baseModel]
    || providerSpecificData?.modelLocations?.[model]
    || providerSpecificData?.locations?.[baseModel]
    || providerSpecificData?.locations?.[model];
  if (perModelOverride) {
    return perModelOverride;
  }

  // 2. Custom connection-level location override (user custom setting)
  const customLoc = configuredLocation || providerSpecificData?.location;
  if (customLoc) {
    // If connection is set to an unsupported regional endpoint (e.g. asia-southeast1),
    // carbon is only available on global, us, or eu multi-region.
    if (/carbon/i.test(baseModel) && /^(asia-|australia-|southamerica-|europe-west[0-9])/i.test(customLoc)) {
      return "global";
    }
    return customLoc;
  }

  // 3. Fallback to model registry default or "global"
  const meta = getModelMeta(provider, model);
  return meta?.defaultLocation || "global";
}

/**
 * VertexExecutor - Google Cloud Vertex AI
 *
 * "vertex"         → Gemini models via regional/global Vertex endpoint
 * "vertex-partner" → Partner models (Llama, Mistral, GLM, DeepSeek, Qwen)
 *                    via global OpenAI-compatible endpoint
 *
 * Auth: SA JSON (stored as apiKey) → JWT assertion → Bearer token (via jose)
 * Token is minted/cached in tokenRefresh.js, not here.
 */
export class VertexExecutor extends BaseExecutor {
  constructor(providerId = "vertex") {
    super(providerId, PROVIDERS[providerId] || {});
  }

  buildUrl(model, stream, urlIndex = 0, credentials = null) {
    const saJson = parseVertexSaJson(credentials?.apiKey);
    const adcJson = parseVertexAdcJson(credentials?.apiKey);
    const usesOAuth = !!saJson || !!adcJson || !!credentials?.accessToken;
    const rawKey = !usesOAuth ? credentials?.apiKey : null;
    const projectId =
      saJson?.project_id ||
      adcJson?.quota_project_id ||
      credentials?.providerSpecificData?.projectId ||
      (rawKey ? projectIdCache.get(rawKey) : null);

    if (this.provider === "vertex-partner") {
      // Partner models require project_id in path regardless of auth method
      if (!projectId) throw new Error("Vertex partner models require a project_id. Add it in providerSpecificData or use Service Account JSON.");
      const url = `https://aiplatform.googleapis.com/v1/projects/${projectId}/locations/global/endpoints/openapi/chat/completions`;
      return rawKey ? `${url}?key=${rawKey}` : url;
    }

    // Gemini on Vertex
    const action = stream ? "streamGenerateContent" : "generateContent";

    if (usesOAuth) {
      // SA JSON / ADC / pre-set accessToken: must use project-scoped path to avoid RESOURCE_PROJECT_INVALID
      if (!projectId) {
        throw new Error(
          "Vertex OAuth/ADC requires a project_id. " +
          "Add quota_project_id to your ADC JSON or set providerSpecificData.projectId."
        );
      }
      // Global-only models (registry forcedLocation/defaultLocation, e.g. Carbon on global)
      // must not route to an unsupported regional location like asia-southeast1.
      const location = resolveVertexLocation(
        this.provider,
        model,
        credentials?.providerSpecificData?.location,
        credentials?.providerSpecificData
      ) || "us-central1";
      let url = `https://aiplatform.googleapis.com/v1/projects/${projectId}/locations/${location}/publishers/google/models/${model}:${action}`;
      if (stream) url += "?alt=sse";
      return url;
    }

    // Raw API keys with an explicit project must honor its location (e.g. Argon on regional, Carbon on global).
    // Keep the projectless endpoint for existing connections without a configured project.
    // ?alt=sse is required for proper SSE streaming (matches every other Gemini executor)
    const location = resolveVertexLocation(
      this.provider,
      model,
      credentials?.providerSpecificData?.location,
      credentials?.providerSpecificData
    ) || "global";
    let url = projectId
      ? `https://aiplatform.googleapis.com/v1/projects/${projectId}/locations/${location}/publishers/google/models/${model}:${action}`
      : `https://aiplatform.googleapis.com/v1/publishers/google/models/${model}:${action}`;
    if (stream) url += "?alt=sse";
    if (rawKey) url += stream ? `&key=${rawKey}` : `?key=${rawKey}`;
    return url;
  }

  buildHeaders(credentials, stream = true) {
    const headers = { "Content-Type": "application/json" };

    // Only set Bearer token if using SA JSON flow (raw key goes in URL ?key=)
    if (credentials.accessToken) {
      headers["Authorization"] = `Bearer ${credentials.accessToken}`;
    }

    if (stream) headers["Accept"] = "text/event-stream";

    return headers;
  }

  async refreshCredentials(credentials, log) {
    const saJson = parseVertexSaJson(credentials?.apiKey);
    if (!saJson) return null;

    const result = await refreshVertexToken(saJson, log);
    if (!result) return null;

    return { accessToken: result.accessToken, expiresAt: result.expiresAt };
  }

  async execute({ model, body, stream, credentials, signal, log, proxyOptions = null }) {
    const saJson = parseVertexSaJson(credentials?.apiKey);
    const adcJson = parseVertexAdcJson(credentials?.apiKey);

    // SA JSON flow: mint Bearer token via JWT assertion (cached)
    if (saJson) {
      const result = await refreshVertexToken(saJson, log);
      if (!result?.accessToken) throw new Error("Vertex: failed to mint access token from Service Account JSON");
      credentials.accessToken = result.accessToken;
    }

    // ADC user credential flow: refresh Bearer token via Google OAuth2 token endpoint
    if (adcJson) {
      const result = await refreshGoogleToken(
        adcJson.refresh_token,
        adcJson.client_id,
        adcJson.client_secret,
        log
      );
      if (!result?.accessToken) throw new Error("Vertex: failed to refresh access token from ADC JSON (authorized_user)");
      credentials.accessToken = result.accessToken;
    }

    // Auto-resolve project_id from raw API key if not provided (both vertex and vertex-partner)
    if (!saJson && !adcJson && !credentials?.providerSpecificData?.projectId && credentials?.apiKey) {
      try {
        const projectId = await resolveProjectId(credentials.apiKey);
        if (projectId) {
          credentials.providerSpecificData = { ...credentials.providerSpecificData, projectId };
          log?.debug?.("VERTEX", `Resolved project_id: ${projectId}`);
        }
      } catch (e) {
        if (this.provider === "vertex-partner") {
          throw new Error("Vertex: could not resolve project_id from API key. Please add it manually in provider settings.");
        }
        log?.warn?.("VERTEX", `Failed to resolve project_id: ${e.message}`);
      }
    }

    const url = this.buildUrl(model, stream, 0, credentials);
    const headers = this.buildHeaders(credentials, stream);
    const transformedBody = this.transformRequest(model, body, stream, credentials);

    const response = await proxyAwareFetch(url, {
      method: "POST",
      headers,
      body: JSON.stringify(transformedBody),
      signal,
    }, proxyOptions);

    return { response, url, headers, transformedBody };
  }
}

export default VertexExecutor;
