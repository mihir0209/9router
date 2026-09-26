import { getCapabilitiesForModel } from "../../../open-sse/providers/capabilities.js";
import { getModelsByProviderId } from "../../../open-sse/config/providerModels.js";
import { getProviderCredentials } from "../../../src/sse/services/auth.js";
import { checkAndRefreshToken } from "../../../src/sse/services/tokenRefresh.js";
import { getProviderConnections } from "../../../src/lib/db/index.js";
import { handleChatCore } from "../../../open-sse/handlers/chatCore.js";

export const RUN_REAL = process.env.RUN_REAL === "1";
export const REAL_TIMEOUT_MS = 90000;
export const CREDENTIAL_SKIP_STATUSES = new Set([401, 402, 403, 429]);

const REAL_PROVIDERS = parseList(process.env.REAL_PROVIDERS);
const REAL_SKIP_PROVIDERS = new Set(parseList(process.env.REAL_SKIP_PROVIDERS));
const REAL_INCLUDE_COMPAT = parseList(process.env.REAL_INCLUDE_COMPAT);

function parseList(value) {
  return String(value || "")
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean);
}

function isCompatibleProvider(provider) {
  return provider.startsWith("openai-compatible-");
}

function includesCompatibleProvider(provider) {
  if (!isCompatibleProvider(provider)) return true;
  return REAL_INCLUDE_COMPAT.includes("1")
    || REAL_INCLUDE_COMPAT.includes("true")
    || REAL_INCLUDE_COMPAT.includes("*")
    || REAL_INCLUDE_COMPAT.includes(provider);
}

function isChatModel(model) {
  const kind = model.kind || model.type || "llm";
  return kind === "llm" || kind === "chat";
}

function hasCapabilities(capabilities, requiredCapabilities) {
  return Object.entries(requiredCapabilities || {}).every(([name, required]) => !required || capabilities[name] === true);
}

export function isCredentialSkip(status) {
  return CREDENTIAL_SKIP_STATUSES.has(Number(status));
}

export function formatChatFailure(target, result) {
  return `${target.provider}/${target.model} failed with ${result.status}: ${result.error || "unknown error"}`;
}

export function requireChatSuccess(context, target, result) {
  if (result.success) return result;
  if (isCredentialSkip(result.status)) {
    context.skip(`${target.provider}/${target.model} credential skip: HTTP ${result.status}`);
  }
  throw new Error(formatChatFailure(target, result));
}

function filterProviderIds(providerIds) {
  return providerIds
    .filter((provider) => REAL_PROVIDERS.length === 0 || REAL_PROVIDERS.includes(provider))
    .filter((provider) => !REAL_SKIP_PROVIDERS.has(provider))
    .filter(includesCompatibleProvider)
    .sort();
}

export async function discoverProviderIds() {
  const connections = await getProviderConnections({ isActive: true });
  return filterProviderIds([...new Set(connections.map((connection) => connection.provider).filter(Boolean))]);
}

export async function prepareTargets({
  requiredCapabilities = {},
  modelPredicate = null,
  providers = null,
  excludeConnectionIds = new Set(),
  refreshCredentials = true,
  limit = Number.POSITIVE_INFINITY,
} = {}) {
  const providerIds = filterProviderIds(providers || await discoverProviderIds());
  const targets = [];
  for (const provider of providerIds) {
    for (const entry of getModelsByProviderId(provider)) {
      if (!isChatModel(entry)) continue;
      const model = entry.id;
      const capabilities = getCapabilitiesForModel(provider, model);
      if (!hasCapabilities(capabilities, requiredCapabilities)) continue;
      if (modelPredicate && !modelPredicate({ provider, model, entry, capabilities })) continue;
      const credentials = await getProviderCredentials(provider, excludeConnectionIds, model);
      if (!credentials || credentials.allRateLimited) continue;
      const refreshedCredentials = refreshCredentials ? await checkAndRefreshToken(provider, credentials) : credentials;
      targets.push({ provider, model, fullModel: `${provider}/${model}`, capabilities, credentials, refreshedCredentials });
      if (targets.length >= limit) return targets;
      break;
    }
  }
  return targets;
}

export async function prepareTarget(options = {}) {
  return (await prepareTargets({ ...options, limit: 1 }))[0] || null;
}

export async function invokeChat(target, body) {
  const result = await handleChatCore({
    body: { model: target.fullModel, ...body },
    modelInfo: { provider: target.provider, model: target.model },
    credentials: target.refreshedCredentials,
    connectionId: target.credentials.connectionId,
  });
  if (!result.success) return result;
  if (body.stream === true) {
    const raw = await result.response.text();
    return { ...result, raw, stream: aggregateStream(parseSse(raw)) };
  }
  const text = await result.response.text();
  let json;
  try {
    json = JSON.parse(text);
  } catch (error) {
    throw new Error(`${target.fullModel} returned invalid JSON: ${error.message}`);
  }
  return { ...result, raw: text, json };
}

/**
 * invokeChat reads the response body to produce `raw`/`json`, which leaves the
 * Response it returns with a spent body. Callers that need to hand the response
 * on (combo fallback passes it straight through) would then hit "Body is
 * unusable: Body has already been read", so rebuild a fresh one from the
 * captured text.
 */
export function replayableResponse(result) {
  if (result?.raw == null) return result?.response;
  return new Response(result.raw, {
    status: result.response?.status || 200,
    headers: { "content-type": "application/json" },
  });
}

export function parseSse(raw) {
  const normalized = String(raw || "").replace(/\r\n?/g, "\n");
  const blocks = normalized.split(/\n{2,}/).filter((block) => block.trim());
  return blocks.map((block) => {
    const lines = block.split("\n");
    const event = lines.find((line) => line.startsWith("event:"))?.slice(6).trim() || "message";
    const data = lines
      .filter((line) => line.startsWith("data:"))
      .map((line) => line.slice(5).trimStart())
      .join("\n");
    let json = null;
    if (data && data !== "[DONE]") {
      try {
        json = JSON.parse(data);
      } catch {}
    }
    return { event, data, json, done: data === "[DONE]" };
  });
}

function textValue(value) {
  if (typeof value === "string") return value;
  if (Array.isArray(value)) return value.map(textValue).join("");
  if (value && typeof value === "object") {
    if (typeof value.text === "string") return value.text;
    if (typeof value.content === "string") return value.content;
  }
  return "";
}

function collectUsage(value) {
  if (!value || typeof value !== "object") return null;
  if (value.usage && typeof value.usage === "object") return value.usage;
  if (value.response?.usage && typeof value.response.usage === "object") return value.response.usage;
  if (value.message?.usage && typeof value.message.usage === "object") return value.message.usage;
  return null;
}

export function aggregateStream(frames) {
  const textParts = [];
  const toolCalls = new Map();
  const finishReasons = [];
  let usage = null;
  let terminal = false;
  for (const frame of frames) {
    const data = frame.json;
    if (!data) {
      if (frame.done) terminal = true;
      continue;
    }
    const choice = data.choices?.[0];
    const delta = choice?.delta || choice?.message || data.delta || {};
    textParts.push(textValue(delta.content ?? data.output_text ?? data.text ?? ""));
    if (data.type === "response.output_text.delta") textParts.push(textValue(data.delta));
    for (const call of delta.tool_calls || data.tool_calls || []) {
      const index = call.index ?? 0;
      const current = toolCalls.get(index) || { id: "", type: "function", function: { name: "", arguments: "" } };
      current.id ||= call.id || "";
      current.type = call.type || current.type;
      current.function.name ||= call.function?.name || call.name || "";
      current.function.arguments += call.function?.arguments ?? call.arguments ?? "";
      toolCalls.set(index, current);
    }
    if (choice?.finish_reason) {
      finishReasons.push(choice.finish_reason);
      terminal = true;
    }
    const type = String(data.type || "");
    const event = String(frame.event || "");
    if (/(?:completed|done|finished|end_turn|message_stop)$/.test(type) || /(?:completed|done|finished|end_turn|message_stop)$/.test(event)) terminal = true;
    usage = collectUsage(data) || usage;
  }
  return {
    text: textParts.join(""),
    toolCalls: [...toolCalls.values()],
    finishReasons,
    usage,
    terminal,
  };
}

export function textFromJson(json) {
  return textValue(json?.choices?.[0]?.message?.content ?? json?.choices?.[0]?.text ?? json?.content ?? json?.output_text ?? "");
}

export function toolCallFromJson(json) {
  return json?.choices?.[0]?.message?.tool_calls?.[0] || json?.tool_calls?.[0] || null;
}

export function validateJsonSchema(value, schema, path = "$" ) {
  if (schema.const !== undefined && value !== schema.const) throw new Error(`${path} does not match const`);
  if (schema.enum && !schema.enum.includes(value)) throw new Error(`${path} is not in enum`);
  if (schema.type === "object") {
    if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`${path} must be an object`);
    for (const key of schema.required || []) {
      if (!Object.hasOwn(value, key)) throw new Error(`${path}.${key} is required`);
    }
    for (const [key, childSchema] of Object.entries(schema.properties || {})) {
      if (Object.hasOwn(value, key)) validateJsonSchema(value[key], childSchema, `${path}.${key}`);
    }
    if (schema.additionalProperties === false) {
      const extras = Object.keys(value).filter((key) => !schema.properties?.[key]);
      if (extras.length) throw new Error(`${path} has unexpected keys: ${extras.join(", ")}`);
    }
  } else if (schema.type === "array") {
    if (!Array.isArray(value)) throw new Error(`${path} must be an array`);
    if (typeof schema.minItems === "number" && value.length < schema.minItems) throw new Error(`${path} has too few items`);
    if (schema.items) value.forEach((item, index) => validateJsonSchema(item, schema.items, `${path}[${index}]`));
  } else if (schema.type === "string" && typeof value !== "string") {
    throw new Error(`${path} must be a string`);
  } else if (schema.type === "string" && typeof schema.minLength === "number" && value.length < schema.minLength) {
    throw new Error(`${path} is too short`);
  } else if (schema.type === "boolean" && typeof value !== "boolean") {
    throw new Error(`${path} must be a boolean`);
  } else if ((schema.type === "number" || schema.type === "integer") && (typeof value !== "number" || Number.isNaN(value))) {
    throw new Error(`${path} must be a number`);
  } else if (schema.type === "null" && value !== null) {
    throw new Error(`${path} must be null`);
  }
  return value;
}

export async function oauthInventory() {
  const connections = await getProviderConnections();
  return connections
    .filter((connection) => connection.authType === "oauth" || connection.refreshToken)
    .map((connection) => ({
      id: connection.id,
      provider: connection.provider,
      authType: connection.authType,
      isActive: Boolean(connection.isActive),
      hasRefreshToken: Boolean(connection.refreshToken),
      expiresAt: connection.expiresAt || null,
      expired: connection.expiresAt ? new Date(connection.expiresAt).getTime() <= Date.now() : null,
    }));
}

export function observedRefreshDecision(credential) {
  const expiresAt = credential?.expiresAt ? new Date(credential.expiresAt).getTime() : null;
  if (!expiresAt) return "observe-without-forcing";
  return expiresAt <= Date.now() ? "expired" : "present";
}
