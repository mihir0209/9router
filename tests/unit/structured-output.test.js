import { describe, it, expect } from "vitest";

import {
  normalizeStructuredOutputMode,
  resolveStructuredOutputMode,
  shouldPreserveJsonSchema,
  NATIVE_JSON_SCHEMA_PROVIDERS,
} from "../../open-sse/config/structuredOutput.js";
import { DefaultExecutor } from "../../open-sse/executors/default.js";
import { AzureExecutor } from "../../open-sse/executors/azure.js";
import "../../open-sse/translator/index.js";
import { translateRequest } from "../../open-sse/translator/index.js";
import { checkFallbackError } from "../../open-sse/services/accountFallback.js";

const SCHEMA = {
  type: "object",
  additionalProperties: false,
  properties: { supplier_name: { type: "string" } },
  required: ["supplier_name"],
};

const mkBody = () => ({
  model: "m",
  messages: [
    { role: "system", content: "sys" },
    { role: "user", content: "hi" },
  ],
  max_completion_tokens: 4096,
  stream: false,
  response_format: {
    type: "json_schema",
    json_schema: { name: "t", strict: true, schema: SCHEMA },
  },
});

describe("structuredOutput mode resolution", () => {
  it("defaults to auto", () => {
    expect(resolveStructuredOutputMode("azure", null)).toBe("auto");
    expect(resolveStructuredOutputMode("openai-compatible-chat-x", null)).toBe("auto");
  });

  it("rejects unknown values back to auto", () => {
    expect(normalizeStructuredOutputMode("bogus")).toBe("auto");
    expect(resolveStructuredOutputMode("azure", { structuredOutputMode: "bogus" })).toBe("auto");
  });

  it("per-request mode beats env/default, per-provider override beats mode", () => {
    expect(resolveStructuredOutputMode("azure", { structuredOutputMode: "fallback" })).toBe("fallback");
    expect(
      resolveStructuredOutputMode("azure", {
        structuredOutputMode: "fallback",
        structuredOutputOverrides: { azure: "passthrough" },
      }),
    ).toBe("passthrough");
  });
});

describe("shouldPreserveJsonSchema (auto defaults per base provider)", () => {
  // Doc-verified native strict support (Sep 2026). Everything else
  // downgrades in auto mode (safe default, overridable).
  it.each(["openai", "azure", "openrouter", "groq", "together", "fireworks", "xai", "mistral", "vertex-partner"])(
    "preserves for known-native base provider %s",
    (provider) => {
      expect(NATIVE_JSON_SCHEMA_PROVIDERS.has(provider)).toBe(true);
      expect(shouldPreserveJsonSchema(provider, null)).toBe(true);
    },
  );

  it.each([
    ["deepseek", "json_object-only chat endpoint"],
    ["xiaomi-mimo", "json_object-only endpoint"],
    ["github", "no conclusive docs (undocumented Copilot API)"],
    ["iflow", "no conclusive docs"],
    ["qoder", "custom SSE agent endpoint, not OpenAI chat"],
    ["opencode", "no conclusive docs"],
    ["opencode-go", "fronts json_object-only models"],
  ])("downgrades %s in auto mode (%s)", (provider) => {
    expect(NATIVE_JSON_SCHEMA_PROVIDERS.has(provider)).toBe(false);
    expect(shouldPreserveJsonSchema(provider, null)).toBe(false);
    expect(shouldPreserveJsonSchema(provider, { structuredOutputMode: "passthrough" })).toBe(true);
  });

  it("downgrades custom openai-compatible-* gateway nodes by default", () => {
    expect(shouldPreserveJsonSchema("openai-compatible-chat-1234", null)).toBe(false);
  });

  it("global passthrough preserves everywhere; global fallback preserves nowhere", () => {
    expect(shouldPreserveJsonSchema("openai-compatible-chat-1234", { structuredOutputMode: "passthrough" })).toBe(true);
    expect(shouldPreserveJsonSchema("azure", { structuredOutputMode: "fallback" })).toBe(false);
    expect(shouldPreserveJsonSchema("openai", { structuredOutputMode: "fallback" })).toBe(false);
  });
});

describe("DefaultExecutor json_schema downgrade policy", () => {
  const chatCreds = { providerSpecificData: { apiType: "chat" } };

  it("keeps json_schema verbatim for azure in auto mode (native support)", () => {
    const out = new AzureExecutor().transformRequest("gpt-5.6-terra", mkBody(), false, chatCreds);
    expect(out.response_format?.type).toBe("json_schema");
    expect(out.response_format?.json_schema?.strict).toBe(true);
    expect(out.messages?.[0]?.content).toBe("sys");
  });

  it("downgrades json_schema for custom gateway nodes in auto mode (legacy)", () => {
    const out = new DefaultExecutor("openai-compatible-chat-1234").transformRequest("m", mkBody(), false, chatCreds);
    expect(out.response_format).toEqual({ type: "json_object" });
    expect(out.messages?.[0]?.content).toContain("strictly follows this JSON schema");
  });

  it("passthrough disables the downgrade for custom gateway nodes", () => {
    const out = new DefaultExecutor("openai-compatible-chat-1234").transformRequest("m", mkBody(), false, {
      ...chatCreds,
      structuredOutputMode: "passthrough",
    });
    expect(out.response_format?.type).toBe("json_schema");
    expect(out.messages?.[0]?.content).toBe("sys");
  });

  it("fallback forces the downgrade even for native providers", () => {
    const out = new DefaultExecutor("azure").transformRequest("m", mkBody(), false, {
      ...chatCreds,
      structuredOutputMode: "fallback",
    });
    expect(out.response_format).toEqual({ type: "json_object" });
  });

  it("downgrades json_schema for json_object-only + undocumented base providers in auto mode", () => {
    for (const provider of ["deepseek", "xiaomi-mimo", "github", "opencode"]) {
      const out = new DefaultExecutor(provider).transformRequest("m", mkBody(), false, chatCreds);
      expect(out.response_format).toEqual({ type: "json_object" });
    }
  });

  it("leaves non-OpenAI-format base providers to their translators", () => {
    // kimi speaks claude format: no verbatim field to preserve, no openai
    // downgrade to apply — the claude translator owns the prompt fallback.
    const out = new DefaultExecutor("kimi").transformRequest("m", mkBody(), false, chatCreds);
    expect(out.response_format?.type).toBe("json_schema");
  });

  it("per-provider override wins over the global mode", () => {
    const out = new DefaultExecutor("openai-compatible-chat-1234").transformRequest("m", mkBody(), false, {
      ...chatCreds,
      structuredOutputMode: "fallback",
      structuredOutputOverrides: { "openai-compatible-chat-1234": "passthrough" },
    });
    expect(out.response_format?.type).toBe("json_schema");
  });
});

describe("native translations (never silently dropped, all modes)", () => {
  const visionBody = () => ({
    model: "m",
    messages: [
      { role: "system", content: "sys" },
      {
        role: "user",
        content: [
          { type: "text", text: "hi" },
          { type: "image_url", image_url: { url: "data:image/jpeg;base64,/9j/FAKE", detail: "high" } },
        ],
      },
    ],
    max_completion_tokens: 4096,
    stream: false,
    response_format: {
      type: "json_schema",
      json_schema: { name: "t", strict: true, schema: SCHEMA },
    },
  });

  it("gemini maps response_format to responseMimeType + responseSchema", () => {
    const out = translateRequest("openai", "gemini", "gemini-2.5-flash", visionBody(), false);
    expect(out.generationConfig.responseMimeType).toBe("application/json");
    expect(out.generationConfig.responseSchema?.properties?.supplier_name).toBeDefined();
    expect(out.response_format).toBeUndefined();
  });

  it("gemini honours max_completion_tokens (previously only max_tokens)", () => {
    const out = translateRequest("openai", "gemini", "gemini-2.5-flash", visionBody(), false);
    expect(out.generationConfig.maxOutputTokens).toBe(4096);
  });

  it("gemini maps plain json_object to responseMimeType", () => {
    const body = visionBody();
    body.response_format = { type: "json_object" };
    const out = translateRequest("openai", "gemini", "gemini-2.5-flash", body, false);
    expect(out.generationConfig.responseMimeType).toBe("application/json");
    expect(out.generationConfig.responseSchema).toBeUndefined();
  });

  it("ollama maps response_format to native format + keeps token limit", () => {
    const out = translateRequest("openai", "ollama", "m", visionBody(), false);
    expect(out.format).toEqual(SCHEMA);
    expect(out.options?.num_predict).toBe(4096);
  });

  it("kiro injects the schema prompt instead of dropping it silently", () => {
    const out = translateRequest("openai", "kiro", "m", visionBody(), false, {
      providerSpecificData: {},
    });
    const text = JSON.stringify(out);
    expect(text).toContain("strictly follows this JSON schema");
  });

  it("claude keeps schema-as-prompt behaviour (only mechanism available)", () => {
    const out = translateRequest("openai", "claude", "claude-opus-4-6", visionBody(), false);
    expect(out.response_format).toBeUndefined();
    expect(JSON.stringify(out.system)).toContain("strictly follows this JSON schema");
  });
});

describe("combo fallback on schema rejections (no hard fail)", () => {
  it.each([
    "Unrecognized request argument supplied: response_format",
    "unknown_parameter: response_format",
    "json_schema is not supported for this model",
  ])("falls back to next combo member on %s", (msg) => {
    expect(checkFallbackError(400, msg, 0, "cool").shouldFallback).toBe(true);
  });

  it("generic 400s still do not fall back (no regression)", () => {
    // "improperly formed request" matched a pre-existing text rule (no
    // shouldFallback field => matcher default true) before this change.
    expect(checkFallbackError(400, "improperly formed request", 0, "cool").shouldFallback).toBe(true);
    expect(checkFallbackError(400, "Bad request", 0, "cool").shouldFallback).toBe(false);
  });
});
