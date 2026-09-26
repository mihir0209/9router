import {
  getProviderCredentials,
  markAccountUnavailable,
  clearAccountError,
} from "../services/auth.js";
import { validateAndEnforceApiKey } from "../services/validateApiKey.js";
import { getModelInfo } from "../services/model.js";
import { handleSystemoneCore } from "open-sse/handlers/systemoneCore.js";
import { errorResponse, unavailableResponse } from "open-sse/utils/error.js";
import { HTTP_STATUS } from "open-sse/config/runtimeConfig.js";
import * as log from "../utils/logger.js";
import { checkAndRefreshToken } from "../services/tokenRefresh.js";
import { saveRequestUsage } from "@/lib/usageDb.js";
import { guardApiKeyResponse } from "../services/apiKeyStreamGuard.js";
import { credentialsErrorResponse, byokUpstreamErrorResponse } from "@/lib/auth/byokError.js";

/**
 * Handle System One (Jev) decision requests for the Next.js server.
 * Follows the same auth + account-fallback pattern as handleEmbeddings.
 *
 * @param {Request} request
 */
export async function handleSystemone(request) {
  let body;
  try {
    body = await request.json();
  } catch {
    log.warn("SYSTEMONE", "Invalid JSON body");
    return errorResponse(HTTP_STATUS.BAD_REQUEST, "Invalid JSON body");
  }

  const url = new URL(request.url);
  const modelStr = body.model;

  log.request("POST", `${url.pathname} | ${modelStr}`);

  // Auth: this route used to do its own boolean `isValidApiKey` check, which
  // bypassed the shared enforcement path entirely. That had three consequences:
  // rate limits, budgets, budgets reservations, model/provider/IP/endpoint
  // restrictions and the concurrency lease were never applied; usage could not
  // be attributed to a key; and because no keyRecord reached
  // getProviderCredentials, a BYOK key silently fell through to a pooled
  // platform account and the platform paid for the call.
  const keyCheck = await validateAndEnforceApiKey(request, { model: modelStr }, "SYSTEMONE");
  if (!keyCheck.allowed) {
    return keyCheck.response;
  }
  const apiKey = keyCheck.apiKey;
  const finish = (response) => response?.body
    ? guardApiKeyResponse(response, keyCheck.keyRecord, keyCheck.release)
    : (void keyCheck.release?.(), response);

  if (!modelStr) {
    log.warn("SYSTEMONE", "Missing model");
    return finish(errorResponse(HTTP_STATUS.BAD_REQUEST, "Missing model"));
  }
  if (body.state === undefined || body.state === null) {
    return finish(errorResponse(HTTP_STATUS.BAD_REQUEST, "Missing required field: state"));
  }
  if (!body.questions || typeof body.questions !== "object" || Array.isArray(body.questions)) {
    return finish(errorResponse(HTTP_STATUS.BAD_REQUEST, "Missing required field: questions"));
  }

  const modelInfo = await getModelInfo(modelStr);
  if (!modelInfo.provider) {
    log.warn("SYSTEMONE", "Invalid model format", { model: modelStr });
    return finish(errorResponse(HTTP_STATUS.BAD_REQUEST, "Invalid model format"));
  }

  const { provider, model } = modelInfo;

  if (modelStr !== `${provider}/${model}`) {
    log.info("ROUTING", `${modelStr} → ${provider}/${model}`);
  } else {
    log.info("ROUTING", `Provider: ${provider}, Model: ${model}`);
  }

  // Credential + fallback loop (mirrors handleEmbeddings)
  const excludeConnectionIds = new Set();
  let lastError = null;
  let lastStatus = null;

  while (true) {
    // keyCheck carries the keyRecord, so a BYOK key is honoured here.
    const credentials = await getProviderCredentials(provider, excludeConnectionIds, model, keyCheck);
    // Must run before the !credentials check: getProviderCredentials *returns* a
    // ByokCredentialsError rather than throwing, and it is truthy, so testing
    // !credentials first would turn a BYOK auth failure into a misleading 400.
    const credentialsError = typeof credentialsErrorResponse === "function" ? credentialsErrorResponse(credentials) : null;
    if (credentialsError) return finish(credentialsError);

    // All accounts unavailable
    if (!credentials || credentials.allRateLimited) {
      if (credentials?.allRateLimited) {
        const errorMsg = lastError || credentials.lastError || "Unavailable";
        const status = lastStatus || Number(credentials.lastErrorCode) || HTTP_STATUS.SERVICE_UNAVAILABLE;
        log.warn("SYSTEMONE", `[${provider}/${model}] ${errorMsg} (${credentials.retryAfterHuman})`);
        return finish(unavailableResponse(status, `[${provider}/${model}] ${errorMsg}`, credentials.retryAfter, credentials.retryAfterHuman));
      }
      if (excludeConnectionIds.size === 0) {
        log.error("AUTH", `No credentials for provider: ${provider}`);
        return finish(errorResponse(HTTP_STATUS.BAD_REQUEST, `No credentials for provider: ${provider}`));
      }
      log.warn("SYSTEMONE", "No more accounts available", { provider });
      return finish(errorResponse(lastStatus || HTTP_STATUS.SERVICE_UNAVAILABLE, lastError || "All accounts unavailable"));
    }

    log.info("AUTH", `\x1b[32mUsing ${provider} account: ${credentials.connectionName}\x1b[0m`);

    const refreshedCredentials = await checkAndRefreshToken(provider, credentials);

    const result = await handleSystemoneCore({
      body,
      modelInfo: { provider, model },
      credentials: refreshedCredentials,
      log,
      onRequestSuccess: async () => {
        await clearAccountError(credentials.connectionId, credentials, model);
      }
    });

    // A BYOK credential rejected upstream is the caller's own key being
    // refused, not a platform account to rotate away from.
    const byokError = byokUpstreamErrorResponse(credentials, result.status);
    if (byokError) return finish(byokError);

    if (result.success) {
      if (result.usage) {
        saveRequestUsage({
          provider,
          model,
          connectionId: credentials.connectionId,
          apiKey,
          apiKeyId: keyCheck.keyRecord?.id || null,
          endpoint: url.pathname,
          tokens: {
            ...result.usage,
            total_tokens: result.usage.prompt_tokens + result.usage.completion_tokens,
          },
          status: "success",
        }).catch(() => {});
      }
      return finish(result.response);
    }

    const { shouldFallback } = await markAccountUnavailable(credentials.connectionId, result.status, result.error, provider, model);

    if (shouldFallback) {
      log.warn("AUTH", `Account ${credentials.connectionName} unavailable (${result.status}), trying fallback`);
      excludeConnectionIds.add(credentials.connectionId);
      lastError = result.error;
      lastStatus = result.status;
      continue;
    }

    return finish(result.response);
  }
}
