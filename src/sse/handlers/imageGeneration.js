import {
  getProviderCredentials,
  markAccountUnavailable,
  clearAccountError,
  extractApiKey,
  isValidApiKey,
} from "../services/auth.js";
import { validateAndEnforceApiKey } from "../services/validateApiKey.js";
import { getSettings } from "@/lib/localDb";
import { getModelInfo, getComboModels } from "../services/model.js";
import { handleImageGenerationCore } from "open-sse/handlers/imageGenerationCore.js";
import { errorResponse, unavailableResponse } from "open-sse/utils/error.js";
import { HTTP_STATUS } from "open-sse/config/runtimeConfig.js";
import { updateProviderCredentials, checkAndRefreshToken } from "../services/tokenRefresh.js";
import { handleComboChat } from "open-sse/services/combo.js";
import * as log from "../utils/logger.js";
import { stripRequestUser } from "@/lib/requestPrivacy.js";
import { guardApiKeyResponse } from "../services/apiKeyStreamGuard.js";
import { credentialsErrorResponse, byokUpstreamErrorResponse } from "@/lib/auth/byokError.js";

// Providers that don't require credentials (noAuth)
const NO_AUTH_PROVIDERS = new Set(["sdwebui", "comfyui"]);

/**
 * Handle image generation request
 * @param {Request} request
 */
export async function handleImageGeneration(request) {
  let body;
  try {
    body = await request.json();
  } catch {
    return errorResponse(HTTP_STATUS.BAD_REQUEST, "Invalid JSON body");
  }

  const url = new URL(request.url);
  const preferredConnectionId = request.headers.get("x-connection-id") || null;
  const wantsStream = (request.headers.get("accept") || "").includes("text/event-stream");
  const binaryOutput = url.searchParams.get("response_format") === "binary";
  const modelStr = body.model;

  const apiKey = extractApiKey(request);
  const settings = await getSettings();
  const keyCheck = await validateAndEnforceApiKey(request, body, "IMAGE");
  if (!keyCheck.allowed) {
    return keyCheck.response;
  }
  body = stripRequestUser(body, settings.stripRequestUser === true);
  const finish = (response) => response?.body
    ? guardApiKeyResponse(response, keyCheck.keyRecord, keyCheck.release)
    : (void keyCheck.release?.(), response);

  if (!modelStr) return finish(errorResponse(HTTP_STATUS.BAD_REQUEST, "Missing model"));
  if (!body.prompt) return finish(errorResponse(HTTP_STATUS.BAD_REQUEST, "Missing required field: prompt"));

  // Combo expansion: model may be a combo name → run fallback/round-robin across models
  const comboModels = await getComboModels(modelStr);
  if (comboModels) {
    const comboStrategies = settings.comboStrategies || {};
    const comboStrategy = comboStrategies[modelStr]?.fallbackStrategy || settings.comboStrategy || "fallback";
    const comboStickyLimit = settings.comboStickyRoundRobinLimit;
    log.info("IMAGE", `Combo "${modelStr}" with ${comboModels.length} models (strategy: ${comboStrategy}, sticky: ${comboStickyLimit})`);
    return handleComboChat({
      body,
      models: comboModels,
      handleSingleModel: (b, m) => handleSingleModelImage(b, m, { wantsStream, binaryOutput, preferredConnectionId, keyContext: keyCheck }),
      log,
      comboName: modelStr,
      comboStrategy,
      comboStickyLimit,
    });
  }

  return handleSingleModelImage(body, modelStr, { wantsStream, binaryOutput, preferredConnectionId, keyContext: keyCheck });
}

async function handleSingleModelImage(body, modelStr, { wantsStream, binaryOutput, preferredConnectionId, keyContext } = {}) {
  const finish = (response) => response?.body
    ? guardApiKeyResponse(response, keyContext.keyRecord, keyContext.release)
    : (void keyContext.release?.(), response);
  const modelInfo = await getModelInfo(modelStr);
  if (!modelInfo.provider) return finish(errorResponse(HTTP_STATUS.BAD_REQUEST, "Invalid model format"));

  const { provider, model } = modelInfo;

  // noAuth providers — no credential needed
  if (NO_AUTH_PROVIDERS.has(provider) && keyContext?.keyRecord?.billingMode !== "byok") {
    const result = await handleImageGenerationCore({
      body,
      modelInfo: { provider, model },
      credentials: null,
      binaryOutput,
    });
    if (result.success) return finish(result.response);
    return finish(errorResponse(result.status || HTTP_STATUS.BAD_GATEWAY, result.error || "Image generation failed"));
  }

  // Credentialed providers — fallback loop
  const excludeConnectionIds = new Set();
  let lastError = null;
  let lastStatus = null;

  while (true) {
    const credentials = await getProviderCredentials(provider, excludeConnectionIds, model, { preferredConnectionId, ...keyContext });
    const credentialsError = typeof credentialsErrorResponse === "function" ? credentialsErrorResponse(credentials) : null;
    if (credentialsError) return finish(credentialsError);

    if (!credentials || credentials.allRateLimited) {
      if (credentials?.allRateLimited) {
        const errorMsg = lastError || credentials.lastError || "Unavailable";
        const status = lastStatus || Number(credentials.lastErrorCode) || HTTP_STATUS.SERVICE_UNAVAILABLE;
        return finish(unavailableResponse(status, `[${provider}/${model}] ${errorMsg}`, credentials.retryAfter, credentials.retryAfterHuman));
      }
      if (excludeConnectionIds.size === 0) {
        return finish(errorResponse(HTTP_STATUS.BAD_REQUEST, `No credentials for provider: ${provider}`));
      }
      return finish(errorResponse(lastStatus || HTTP_STATUS.SERVICE_UNAVAILABLE, lastError || "All accounts unavailable"));
    }

    const refreshedCredentials = await checkAndRefreshToken(provider, credentials);

    const result = await handleImageGenerationCore({
      body,
      modelInfo: { provider, model },
      credentials: refreshedCredentials,
      streamToClient: wantsStream,
      binaryOutput,
      onCredentialsRefreshed: async (newCreds) => {
        await updateProviderCredentials(credentials.connectionId, {
          accessToken: newCreds.accessToken,
          refreshToken: newCreds.refreshToken,
          providerSpecificData: newCreds.providerSpecificData,
          testStatus: "active"
        });
      },
      onRequestSuccess: async () => {
        await clearAccountError(credentials.connectionId, credentials, model);
      }
    });

    if (result.success) return finish(result.response);

    const byokError = byokUpstreamErrorResponse(credentials, result.status);
    if (byokError) return finish(byokError);
    const { shouldFallback } = await markAccountUnavailable(credentials.connectionId, result.status, result.error, provider, model);

    if (shouldFallback) {
      excludeConnectionIds.add(credentials.connectionId);
      lastError = result.error;
      lastStatus = result.status;
      continue;
    }

    return finish(result.response);
  }
}
