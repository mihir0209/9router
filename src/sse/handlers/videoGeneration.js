import {
  getProviderCredentials,
  markAccountUnavailable,
  clearAccountError,
  extractApiKey,
} from "../services/auth.js";
import { validateAndEnforceApiKey } from "../services/validateApiKey.js";
import { resolveProviderId } from "@/shared/constants/providers";
import { getProviderConnectionById, getSettings } from "@/lib/localDb";
import { getModelInfo } from "../services/model.js";
import { handleVideoProxyCore, getVideoConfig, sanitizeSecrets } from "open-sse/handlers/videoCore.js";
import { errorResponse, unavailableResponse } from "open-sse/utils/error.js";
import { HTTP_STATUS } from "open-sse/config/runtimeConfig.js";
import { updateProviderCredentials, checkAndRefreshToken } from "../services/tokenRefresh.js";
import * as log from "../utils/logger.js";
import { stripRequestUser } from "@/lib/requestPrivacy.js";
import { guardApiKeyResponse } from "../services/apiKeyStreamGuard.js";
import { credentialsErrorResponse, byokUpstreamErrorResponse } from "@/lib/auth/byokError.js";

// Video generation is xAI-only today; requests without a provider prefix
// (bare model id, or multipart bodies we deliberately don't parse) land here.
const DEFAULT_VIDEO_PROVIDER = "xai";

/**
 * Poll requests carry no model, so the provider comes from the pinned
 * connection (`x-connection-id`, returned on create) or an explicit
 * `?provider=` — falling back to the historical xAI default.
 *
 * `keyRecord` is needed because a BYOK key's "connection" is the synthetic id
 * `byok:<keyId>`, which is not a providerConnections row: looking it up always
 * missed and the poll silently fell back to xAI, so a BYOK key configured for
 * another video provider was rejected by the strict provider check on poll even
 * though its create call had succeeded.
 */
async function resolveGetProvider(request, connectionId, keyRecord) {
  if (connectionId?.startsWith("byok:") && keyRecord?.byokProvider) {
    const byokProvider = resolveProviderId(keyRecord.byokProvider);
    if (getVideoConfig(byokProvider)) return byokProvider;
  }
  if (connectionId) {
    const conn = await getProviderConnectionById(connectionId).catch(() => null);
    if (conn?.provider && getVideoConfig(conn.provider)) return conn.provider;
  }
  const queried = new URL(request.url).searchParams.get("provider");
  if (queried && getVideoConfig(queried)) return queried;
  return DEFAULT_VIDEO_PROVIDER;
}

// Creation POSTs are billable jobs — only rotate to another account for
// errors that upstream rejects BEFORE creating a job (auth/quota). A 5xx may
// have created the job, so it is returned to the caller instead of re-sent.
const CREATE_ROTATION_STATUSES = new Set([
  HTTP_STATUS.UNAUTHORIZED,
  HTTP_STATUS.FORBIDDEN,
  HTTP_STATUS.RATE_LIMITED,
]);

async function requireValidApiKey(request) {
  return validateAndEnforceApiKey(request, {}, "VIDEO");
}

function finishKeyResponse(response, keyCheck) {
  return response?.body
    ? guardApiKeyResponse(response, keyCheck.keyRecord, keyCheck.release)
    : (void keyCheck.release?.(), response);
}

/**
 * Read the request body once, byte-preserving.
 * JSON bodies are additionally parsed so the `model` provider prefix can be
 * resolved (and stripped) — everything else is forwarded exactly as received.
 */
async function readForwardableBody(request) {
  const contentType = request.headers.get("content-type") || "";
  if (contentType.includes("application/json")) {
    const raw = await request.text();
    let parsed;
    try {
      parsed = JSON.parse(raw);
    } catch {
      return { error: errorResponse(HTTP_STATUS.BAD_REQUEST, "Invalid JSON body") };
    }
    return { raw, parsed, contentType };
  }
  // Multipart (or any other content type): forward the exact bytes — parsing
  // and re-encoding FormData would change the multipart boundary.
  const buf = Buffer.from(await request.arrayBuffer());
  return { raw: buf, parsed: null, contentType };
}

async function resolveVideoProvider(parsedBody) {
  if (!parsedBody?.model) return { provider: DEFAULT_VIDEO_PROVIDER, model: null };

  const modelStr = String(parsedBody.model);
  const modelInfo = await getModelInfo(modelStr);
  if (!modelInfo.provider) {
    return { error: errorResponse(HTTP_STATUS.BAD_REQUEST, "Combos are not supported for video generation") };
  }
  if (!getVideoConfig(modelInfo.provider)) {
    // Bare model ids (no explicit "provider/" prefix) fall back to the default
    // video provider — the prefix-less inference targets chat providers only.
    if (!modelStr.includes("/")) {
      return { provider: DEFAULT_VIDEO_PROVIDER, model: modelStr };
    }
    return { error: errorResponse(HTTP_STATUS.BAD_REQUEST, `Provider '${modelInfo.provider}' does not support video generation`) };
  }
  return { provider: modelInfo.provider, model: modelInfo.model };
}

function withConnectionHeader(response, connectionId) {
  if (!connectionId) return response;
  const headers = new Headers(response.headers);
  // Video jobs are account-bound upstream — clients echo this back as
  // `x-connection-id` on GET polls so the same account is used.
  headers.set("x-9router-connection-id", String(connectionId));
  return new Response(response.body, { status: response.status, headers });
}

/**
 * POST /v1/videos/{generations|edits|extensions} — async job creation proxy.
 */
export async function handleVideoCreate(request, action) {
  const keyCheck = await requireValidApiKey(request);
  if (!keyCheck.allowed) return keyCheck.response;

  const bodyInfo = await readForwardableBody(request);
  if (bodyInfo.error) return finishKeyResponse(bodyInfo.error, keyCheck);
  const settings = await getSettings();
  if (bodyInfo.parsed) {
    const sanitized = stripRequestUser(bodyInfo.parsed, settings.stripRequestUser === true);
    if (sanitized !== bodyInfo.parsed) {
      bodyInfo.parsed = sanitized;
      bodyInfo.raw = JSON.stringify(sanitized);
    }
  }

  const resolved = await resolveVideoProvider(bodyInfo.parsed);
  if (resolved.error) return finishKeyResponse(resolved.error, keyCheck);
  const { provider, model } = resolved;

  // Strip the provider prefix (e.g. "xai/grok-imagine-video") before forwarding;
  // otherwise forward the original bytes untouched.
  let forwardBody = bodyInfo.raw;
  if (bodyInfo.parsed && model && bodyInfo.parsed.model !== model) {
    forwardBody = JSON.stringify({ ...bodyInfo.parsed, model });
  }

  const preferredConnectionId = request.headers.get("x-connection-id") || null;
  const idempotencyKey = request.headers.get("idempotency-key") || null;

  const excludeConnectionIds = new Set();
  let lastError = null;
  let lastStatus = null;

  while (true) {
    const credentials = await getProviderCredentials(provider, excludeConnectionIds, model, { preferredConnectionId, ...keyCheck });
    const credentialsError = typeof credentialsErrorResponse === "function" ? credentialsErrorResponse(credentials) : null;
    if (credentialsError) return finishKeyResponse(credentialsError, keyCheck);

    if (!credentials || credentials.allRateLimited) {
      if (credentials?.allRateLimited) {
        const errorMsg = lastError || credentials.lastError || "Unavailable";
        const status = lastStatus || Number(credentials.lastErrorCode) || HTTP_STATUS.SERVICE_UNAVAILABLE;
        return finishKeyResponse(unavailableResponse(status, `[${provider}/${model || "video"}] ${errorMsg}`, credentials.retryAfter, credentials.retryAfterHuman), keyCheck);
      }
      if (excludeConnectionIds.size === 0) {
        return finishKeyResponse(errorResponse(HTTP_STATUS.BAD_REQUEST, `No credentials for provider: ${provider}`), keyCheck);
      }
      return finishKeyResponse(errorResponse(lastStatus || HTTP_STATUS.SERVICE_UNAVAILABLE, lastError || "All accounts unavailable"), keyCheck);
    }

    const refreshedCredentials = await checkAndRefreshToken(provider, credentials);

    const result = await handleVideoProxyCore({
      provider,
      action,
      rawBody: forwardBody,
      contentType: bodyInfo.contentType || null,
      idempotencyKey,
      credentials: refreshedCredentials,
      signal: request.signal,
      log,
      onCredentialsRefreshed: async (newCreds) => {
        await updateProviderCredentials(credentials.connectionId, {
          accessToken: newCreds.accessToken,
          refreshToken: newCreds.refreshToken,
          providerSpecificData: newCreds.providerSpecificData,
          testStatus: "active",
        });
      },
    });

    if (result.success) {
      await clearAccountError(credentials.connectionId, credentials, model);
      log.info("VIDEO", `${provider.toUpperCase()} | ${action} accepted (connection ${credentials.connectionId})`);
      return finishKeyResponse(withConnectionHeader(result.response, credentials.connectionId), keyCheck);
    }

    const byokError = byokUpstreamErrorResponse(credentials, result.status);
    if (byokError) return finishKeyResponse(byokError, keyCheck);
    // Record the failure (dashboard shows lastError/errorCode → user sees re-auth is needed)
    const { shouldFallback } = await markAccountUnavailable(
      credentials.connectionId, result.status, sanitizeSecrets(result.error, refreshedCredentials), provider, model
    );

    if (shouldFallback && CREATE_ROTATION_STATUSES.has(result.status)) {
      excludeConnectionIds.add(credentials.connectionId);
      lastError = result.error;
      lastStatus = result.status;
      continue;
    }

    return finishKeyResponse(result.response, keyCheck);
  }
}

/**
 * GET /v1/videos/{request_id} — poll job status.
 * Jobs are account-bound upstream, so no cross-account rotation here: the
 * caller pins the creating account via `x-connection-id` (returned on create).
 */
export async function handleVideoGet(request, requestId) {
  const keyCheck = await requireValidApiKey(request);
  if (!keyCheck.allowed) return keyCheck.response;

  if (!requestId) return finishKeyResponse(errorResponse(HTTP_STATUS.BAD_REQUEST, "Missing video request id"), keyCheck);

  const preferredConnectionId = request.headers.get("x-connection-id") || null;
  const provider = await resolveGetProvider(request, preferredConnectionId, keyCheck.keyRecord);

  const credentials = await getProviderCredentials(provider, null, null, { preferredConnectionId, ...keyCheck });
  const credentialsError = credentialsErrorResponse(credentials);
  if (credentialsError) return finishKeyResponse(credentialsError, keyCheck);
  if (!credentials || credentials.allRateLimited) {
    return finishKeyResponse(errorResponse(HTTP_STATUS.BAD_REQUEST, `No credentials for provider: ${provider}`), keyCheck);
  }

  const refreshedCredentials = await checkAndRefreshToken(provider, credentials);

  const result = await handleVideoProxyCore({
    provider,
    requestId,
    credentials: refreshedCredentials,
    signal: request.signal,
    log,
    onCredentialsRefreshed: async (newCreds) => {
      await updateProviderCredentials(credentials.connectionId, {
        accessToken: newCreds.accessToken,
        refreshToken: newCreds.refreshToken,
        providerSpecificData: newCreds.providerSpecificData,
        testStatus: "active",
      });
    },
  });

  if (result.success) {
    await clearAccountError(credentials.connectionId, credentials, null);
    return finishKeyResponse(withConnectionHeader(result.response, credentials.connectionId), keyCheck);
  }

  const byokError = byokUpstreamErrorResponse(credentials, result.status);
  if (byokError) return finishKeyResponse(byokError, keyCheck);
  await markAccountUnavailable(
    credentials.connectionId, result.status, sanitizeSecrets(result.error, refreshedCredentials), provider, null
  );
  return finishKeyResponse(result.response, keyCheck);
}
