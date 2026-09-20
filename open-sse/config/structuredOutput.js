// Structured-output (response_format / json_schema) handling policy.
//
// Problem: 9router accepts OpenAI's `response_format: { type: "json_schema", ... }`
// on ingress but silently downgraded or dropped it on egress, depending on the
// target provider. This module is the single source of truth for that policy.
//
// Modes (global `structuredOutputMode`, optional per-provider overrides):
//   "auto"         (default) Preserve/translate natively where the provider is
//                  known to support it; prompt-injection fallback otherwise.
//                  Zero new 400 risk: combo fallback behaviour is unchanged.
//   "passthrough"  Never downgrade: forward `response_format` verbatim to
//                  OpenAI-format targets, translate to the native equivalent
//                  for Gemini/Ollama/Responses targets. Upstreams that reject
//                  it answer 400, which the retry matcher classifies as
//                  combo-fallbackable (see ERROR_RULES response_format rules),
//                  so the next combo member is tried instead of hard-failing.
//   "fallback"     Legacy behaviour: always downgrade json_schema to
//                  json_object + system-prompt schema injection, for every
//                  provider. Maximum compatibility, no enforcement.
//
// Custom `openai-compatible-*` nodes (user-added gateways) default to the
// fallback in "auto" mode: most such gateways don't implement Structured
// Output, and a verbatim forward would 400. Set the global mode to
// "passthrough" (or an override for that node id) once you know the gateway
// behind it supports response_format.

export const STRUCTURED_OUTPUT_MODES = ["auto", "passthrough", "fallback"];

export const STRUCTURED_OUTPUT_MODE = {
  AUTO: "auto",
  PASSTHROUGH: "passthrough",
  FALLBACK: "fallback",
};

// Base registry providers whose OpenAI-format endpoint natively honors
// `response_format: { type: "json_schema", json_schema: { strict, schema } }`.
// Verbatim preserve is safe for these in "auto" mode. Membership verified
// against official docs (Sep 2026); providers with only json_object support
// or no conclusive docs are deliberately EXCLUDED and get the safe downgrade
// instead (overridable per-provider / globally via "passthrough").
//
// Kept OUT on purpose:
//   deepseek, xiaomi-mimo — chat endpoints speak only { type: "json_object" }
//     (DeepSeek strict schema lives on /beta + /responses only; MiMo
//     validates schemas client-side). See JSON_OBJECT_ONLY_PROVIDERS.
//   github (Copilot API), iflow, qoder, opencode, opencode-go — no conclusive
//     public docs for native json_schema passthrough; several front models
//     that are json_object-only. Downgrade is the safe default here.
// Docs: OpenAI structured-outputs, Azure foundry structured-outputs,
// OpenRouter structured-outputs, Groq structured-outputs, Together json-mode,
// Fireworks structured-response-formatting, xAI structured-outputs,
// Mistral custom structured-output, Vertex MaaS structured-output.
export const NATIVE_JSON_SCHEMA_PROVIDERS = new Set([
  "openai",
  "azure",
  "openrouter",
  "groq",
  "together",
  "fireworks",
  "xai",
  "mistral",
  "vertex-partner",
]);

// Base providers whose OpenAI-compatible chat endpoint supports
// { type: "json_object" } but NOT strict json_schema. In "auto" mode a
// json_schema request is downgraded to json_object + prompt injection —
// enforcement is lost, but the request won't 400 on the strict shape.
export const JSON_OBJECT_ONLY_PROVIDERS = new Set([
  "deepseek",
  "xiaomi-mimo",
]);

// Providers translated to their NATIVE structured-output equivalent instead of
// the OpenAI wire shape (handled in their translators, all modes):
//   gemini / vertex / gemini-cli / antigravity -> generationConfig.responseMimeType
//     + responseSchema (Gemini native constrained decoding)
//   ollama / ollama-local -> top-level `format` (json-schema passthrough)
//   codex / grok-cli (openai-responses) -> text.format (already existed)
export const NATIVE_TRANSLATED_FORMATS = new Set([
  "gemini",
  "vertex",
  "gemini-cli",
  "antigravity",
  "ollama",
  "ollama-local",
]);

// Claude-family formats have no native response_format equivalent (Anthropic
// Messages API constrains JSON only via forced tool-use). Prompt injection is
// the only mechanism, in every mode.
export const PROMPT_FALLBACK_FORMATS = new Set(["claude"]);

const OPENAI_COMPATIBLE_PREFIX = "openai-compatible-";

export function normalizeStructuredOutputMode(value, fallback = STRUCTURED_OUTPUT_MODE.AUTO) {
  return STRUCTURED_OUTPUT_MODES.includes(value) ? value : fallback;
}

// Effective mode for one request. Precedence:
//   1. per-provider override for this exact provider/node id
//   2. per-request credentials flag (set by the app layer from DB settings)
//   3. process.env.STRUCTURED_OUTPUT_MODE (standalone engine use)
//   4. "auto"
export function resolveStructuredOutputMode(provider, credentials = null) {
  const overrides = credentials?.structuredOutputOverrides;
  if (overrides && typeof overrides === "object" && overrides[provider] !== undefined) {
    const m = normalizeStructuredOutputMode(overrides[provider], null);
    if (m) return m;
  }
  if (credentials?.structuredOutputMode !== undefined) {
    const m = normalizeStructuredOutputMode(credentials.structuredOutputMode, null);
    if (m) return m;
  }
  const env = typeof process !== "undefined" ? process.env?.STRUCTURED_OUTPUT_MODE : null;
  if (env) {
    const m = normalizeStructuredOutputMode(String(env).toLowerCase().trim(), null);
    if (m) return m;
  }
  return STRUCTURED_OUTPUT_MODE.AUTO;
}

// Should an OpenAI-format target receive `response_format` verbatim?
//   passthrough -> always; fallback -> never;
//   auto -> yes for known-native base providers, no for custom
//   openai-compatible-* nodes (unknown gateway capabilities).
export function shouldPreserveJsonSchema(provider, credentials = null) {
  const mode = resolveStructuredOutputMode(provider, credentials);
  if (mode === STRUCTURED_OUTPUT_MODE.PASSTHROUGH) return true;
  if (mode === STRUCTURED_OUTPUT_MODE.FALLBACK) return false;
  if (typeof provider === "string" && provider.startsWith(OPENAI_COMPATIBLE_PREFIX)) return false;
  return NATIVE_JSON_SCHEMA_PROVIDERS.has(provider);
}

// Shared prompt-injection fallback text. One wording for every path that must
// beg for JSON instead of enforcing it (DefaultExecutor custom nodes, Claude,
// Kiro). Callers splice in the stringified schema.
export function buildJsonSchemaPrompt(schemaJson) {
  return (
    `You must respond with valid JSON that strictly follows this JSON schema:\n` +
    `\`\`\`json\n${schemaJson}\n\`\`\`\n` +
    `Respond ONLY with the JSON object, no other text.`
  );
}

export function extractJsonSchema(body) {
  const rf = body?.response_format;
  if (rf?.type === "json_schema" && rf.json_schema?.schema) return rf.json_schema.schema;
  return null;
}

// Pure downgrade: json_schema -> json_object + system-prompt schema injection.
// Returns the original body untouched when there is nothing to downgrade.
// Shared by DefaultExecutor (custom gateways + non-native bases) and
// GithubExecutor (undocumented Copilot API: safe default, overridable).
export function downgradeJsonSchemaToJsonObject(body) {
  const schema = extractJsonSchema(body);
  if (!schema) return body;

  const schemaJson = JSON.stringify(schema, null, 2);
  const prompt = buildJsonSchemaPrompt(schemaJson);

  const messages = Array.isArray(body.messages) ? body.messages.map(m => ({ ...m })) : [];
  const sys = messages.find(m => m.role === "system");
  if (sys) {
    if (typeof sys.content === "string") sys.content = `${sys.content}\n\n${prompt}`;
    else if (Array.isArray(sys.content)) sys.content.push({ type: "text", text: `\n\n${prompt}` });
  } else {
    messages.unshift({ role: "system", content: prompt });
  }
  return { ...body, messages, response_format: { type: "json_object" } };
}
