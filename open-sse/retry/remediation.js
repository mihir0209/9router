import { errorTextIncludes } from "./schema.js";
import { openaiToOpenAIResponsesRequest } from "../translator/request/openai-responses.js";

/**
 * Registry of dynamic request remediation handlers.
 * When upstream fails with a specific error (e.g. 400), a registered remediation
 * hook can mutate the request payload, URL, headers, or target format in-flight
 * and trigger an automatic retry on the same account.
 */
const REMEDIATION_HANDLERS = [];

/**
 * Register a remediation handler.
 * @param {object} handler
 * @param {string|string[]} [handler.pattern] - Substring pattern(s) to match against error body
 * @param {number} [handler.status=400] - Status code to match
 * @param {function} handler.remediate - (ctx) => { transformedBody?, url?, headers?, retry: boolean }
 */
export function registerRemediationHandler(handler) {
  if (!handler || typeof handler.remediate !== "function") return;
  REMEDIATION_HANDLERS.push(handler);
}

// 1. OpenAI / Cloudflare AI Gateway: Function tools with reasoning_effort on /v1/chat/completions
// Error: "Function tools with reasoning_effort are not supported for gpt-5.6-sol in /v1/chat/completions. To use function tools, use /v1/responses or set reasoning_effort to 'none'."
registerRemediationHandler({
  pattern: ["to use function tools, use /v1/responses", "use /v1/responses or set reasoning_effort to 'none'"],
  status: 400,
  remediate: ({ url, transformedBody, headers, stream, credentials, model }) => {
    // If upstream supports switching to /responses
    if (url.includes("/chat/completions")) {
      const responsesUrl = url.replace("/chat/completions", "/responses");
      const responsesBody = openaiToOpenAIResponsesRequest(model || transformedBody.model, transformedBody, stream, credentials);
      return {
        url: responsesUrl,
        transformedBody: responsesBody,
        retry: true,
      };
    }
    // Alternatively, if switching URL is not feasible, downgrade reasoning_effort to 'none'
    if (transformedBody.reasoning_effort) {
      const cloned = { ...transformedBody };
      cloned.reasoning_effort = "none";
      if (cloned.reasoning && typeof cloned.reasoning === "object") {
        cloned.reasoning = { ...cloned.reasoning, effort: "none" };
      }
      return {
        transformedBody: cloned,
        retry: true,
      };
    }
    return null;
  }
});

// 2. Antigravity: Invalid Argument (e.g. invalid thinking configuration or un-tiered model name)
registerRemediationHandler({
  pattern: ["request contains an invalid argument", "invalid_argument"],
  status: 400,
  remediate: ({ transformedBody }) => {
    // If antigravity received non-tiered model name or incompatible generationConfig
    let mutated = false;
    const body = { ...transformedBody };
    if (body.request?.generationConfig) {
      body.request = { ...body.request };
      body.request.generationConfig = { ...body.request.generationConfig };
      // Strip thinkingConfig if rejected
      if (body.request.generationConfig.thinkingConfig) {
        delete body.request.generationConfig.thinkingConfig;
        mutated = true;
      }
    }
    if (mutated) {
      return {
        transformedBody: body,
        retry: true,
      };
    }
    return null;
  }
});

/**
 * Resolve remediation for an upstream error.
 * @param {object} ctx - { status, errorBody, url, transformedBody, headers, stream, credentials, model, provider }
 * @returns {object|null} - { transformedBody?, url?, headers?, retry: boolean }
 */
export function resolveRemediation(ctx) {
  const { status, errorBody } = ctx;
  if (!status || !errorBody) return null;

  for (const handler of REMEDIATION_HANDLERS) {
    if (handler.status && handler.status !== status) continue;
    if (handler.pattern) {
      const patterns = Array.isArray(handler.pattern) ? handler.pattern : [handler.pattern];
      const match = patterns.some(p => errorTextIncludes(errorBody, p));
      if (!match) continue;
    }
    const result = handler.remediate(ctx);
    if (result && result.retry) return result;
  }

  return null;
}
