import { errorTextIncludes } from "./schema.js";
import { budgetToLevel } from "../translator/concerns/thinking.js";

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
 * @param {string|RegExp|(string|RegExp)[]} [handler.pattern] - Pattern(s) to match against error body
 * @param {number} [handler.status=400] - Status code to match
 * @param {function} handler.remediate - (ctx) => { transformedBody?, url?, headers?, retry: boolean }
 */
export function registerRemediationHandler(handler) {
  if (!handler || typeof handler.id !== "string" || !handler.id || typeof handler.remediate !== "function") return;
  REMEDIATION_HANDLERS.push(handler);
}

function matchPattern(errorBody, pattern) {
  if (pattern instanceof RegExp) {
    pattern.lastIndex = 0;
    return pattern.exec(String(errorBody || ""));
  }
  return errorTextIncludes(errorBody, pattern) ? [pattern] : null;
}

function normalizeClaudeEffort(budgetTokens) {
  const level = budgetToLevel(budgetTokens);
  if (level === "xhigh" || level === "max") return "high";
  return level || "medium";
}

function isSafePath(path) {
  const unsafe = new Set(["__proto__", "prototype", "constructor"]);
  return path.split(".").every((part) => part && !unsafe.has(part));
}

function getPath(object, path) {
  return path.split(".").reduce((value, key) => value?.[key], object);
}

function setPath(object, path, value) {
  const keys = path.split(".");
  let current = object;
  for (const key of keys.slice(0, -1)) {
    const child = current[key];
    current[key] = child && typeof child === "object" && !Array.isArray(child) ? { ...child } : {};
    current = current[key];
  }
  const last = keys.at(-1);
  if (current[last] === undefined) current[last] = value;
}

function deletePath(object, path) {
  const keys = path.split(".");
  let current = object;
  for (const key of keys.slice(0, -1)) {
    if (!current[key] || typeof current[key] !== "object") return;
    current[key] = { ...current[key] };
    current = current[key];
  }
  delete current[keys.at(-1)];
}

// Upstreams can explicitly announce a renamed top-level request parameter.
// Capture the names from the response rather than tying this to a provider or model.
registerRemediationHandler({
  id: "rename-unsupported-parameter",
  pattern: /unsupported parameter:\s*\\?["']([A-Za-z_][A-Za-z0-9_.]*)\\?["'][\s\S]*?(?:this\s+)?parameter has moved to\s*\\?["']([A-Za-z_][A-Za-z0-9_.]*)\\?["']/i,
  status: 400,
  remediate: ({ transformedBody, match }) => {
    const [, from, to] = match || [];
    if (!from || !to || from === to || !isSafePath(from) || !isSafePath(to) || getPath(transformedBody, from) === undefined) return null;
    const body = { ...transformedBody };
    setPath(body, to, getPath(transformedBody, from));
    deletePath(body, from);
    return { transformedBody: body, retry: true };
  },
});

// New Claude-compatible APIs can describe the exact legacy and replacement
// thinking types in their error. Apply only when the submitted value matches.
registerRemediationHandler({
  id: "replace-unsupported-claude-thinking-type",
  pattern: /\\?["']thinking\.type\.([A-Za-z][A-Za-z0-9_-]*)\\?["']\s+is not supported[\s\S]*?use\s+\\?["']thinking\.type\.([A-Za-z][A-Za-z0-9_-]*)\\?["'][\s\S]*?\\?["']output_config\.effort\\?["']/i,
  status: 400,
  remediate: ({ transformedBody, match }) => {
    const [, from, to] = match || [];
    if (!from || !to || from === to || transformedBody?.thinking?.type !== from) return null;
    const effort = transformedBody.output_config?.effort
      || normalizeClaudeEffort(transformedBody.thinking.budget_tokens);
    return {
      transformedBody: {
        ...transformedBody,
        thinking: { type: to },
        output_config: { ...(transformedBody.output_config || {}), effort },
      },
      retry: true,
    };
  },
});

// 1. OpenAI / Cloudflare AI Gateway: Function tools with reasoning_effort on /v1/chat/completions
// Error: "Function tools with reasoning_effort are not supported for gpt-5.6-sol in /v1/chat/completions. To use function tools, use /v1/responses or set reasoning_effort to 'none'."
registerRemediationHandler({
  id: "openai-tools-require-responses",
  pattern: ["to use function tools, use /v1/responses", "use /v1/responses or set reasoning_effort to 'none'"],
  status: 400,
  remediate: async ({ url, transformedBody, stream, credentials, model }) => {
    // If upstream supports switching to /responses
    if (url.includes("/chat/completions")) {
      // Lazy import avoids BaseExecutor → remediation → translator registry → executors cycle.
      const { openaiToOpenAIResponsesRequest } = await import("../translator/request/openai-responses.js");
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
  id: "antigravity-invalid-thinking-config",
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
 * @returns {Promise<object|null>} - { transformedBody?, url?, headers?, retry: boolean, handlerId }
 */
export async function resolveRemediation(ctx, attemptedHandlerIds = new Set()) {
  const { status, errorBody } = ctx;
  if (!status || !errorBody) return null;

  for (const handler of REMEDIATION_HANDLERS) {
    if (attemptedHandlerIds.has(handler.id)) continue;
    if (handler.status && handler.status !== status) continue;
    let match = null;
    if (handler.pattern) {
      const patterns = Array.isArray(handler.pattern) ? handler.pattern : [handler.pattern];
      for (const pattern of patterns) {
        match = matchPattern(errorBody, pattern);
        if (match) break;
      }
      if (!match) continue;
    }
    const result = await handler.remediate({ ...ctx, match });
    if (result && result.retry) return { ...result, handlerId: handler.id };
  }

  return null;
}
