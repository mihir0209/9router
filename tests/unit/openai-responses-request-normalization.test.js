import { describe, expect, it } from "vitest";
import { normalizeOpenAIResponsesRequest } from "../../open-sse/translator/formats/responsesApi.js";
import { openaiToOpenAIResponsesRequest } from "../../open-sse/translator/request/openai-responses.js";
import { translateRequest } from "../../open-sse/translator/index.js";

describe("OpenAI Responses request normalization", () => {
  it("canonicalizes documented Chat aliases", () => {
    expect(normalizeOpenAIResponsesRequest({
      input: "hello",
      max_completion_tokens: 100,
      reasoning_effort: "high",
      reasoning_summary: "detailed",
      verbosity: "low",
      response_format: {
        type: "json_schema",
        json_schema: { name: "answer", schema: { type: "object" }, strict: true },
      },
      logprobs: true,
      stream_options: { include_usage: true, include_obfuscation: false },
      tool_choice: { type: "function", function: { name: "lookup" } },
      n: 1,
      stop: ["END"],
      frequency_penalty: 0.5,
      presence_penalty: 0.5,
      logit_bias: { 42: 10 },
      seed: 42,
      modalities: ["text"],
      audio: { voice: "alloy" },
      prediction: { type: "content", content: "hello" },
    })).toEqual({
      input: "hello",
      max_output_tokens: 100,
      reasoning: { effort: "high", summary: "detailed" },
      text: {
        verbosity: "low",
        format: { type: "json_schema", name: "answer", schema: { type: "object" }, strict: true },
      },
      include: ["message.output_text.logprobs"],
      stream_options: { include_obfuscation: false },
      tool_choice: { type: "function", name: "lookup" },
    });
  });

  it("preserves canonical nested fields over legacy aliases", () => {
    expect(normalizeOpenAIResponsesRequest({
      input: "hello",
      max_output_tokens: 7,
      max_tokens: 99,
      reasoning: { effort: "low", summary: "concise" },
      reasoning_effort: "high",
      reasoning_summary: "detailed",
      text: { verbosity: "medium", format: { type: "json_object" } },
      verbosity: "high",
      response_format: { type: "text" },
    })).toEqual({
      input: "hello",
      max_output_tokens: 7,
      reasoning: { effort: "low", summary: "concise" },
      text: { verbosity: "medium", format: { type: "json_object" } },
    });
  });

  it("normalizes an existing Responses body instead of returning aliases unchanged", () => {
    const result = openaiToOpenAIResponsesRequest("demo", {
      input: "hello",
      max_tokens: 11,
      reasoning_effort: "medium",
    }, false, null);

    expect(result.max_output_tokens).toBe(11);
    expect(result.reasoning).toEqual({ effort: "medium" });
    expect(result.max_tokens).toBeUndefined();
    expect(result.reasoning_effort).toBeUndefined();
  });

  it("finishes the full translation pipeline in canonical Responses format", () => {
    const result = translateRequest("openai", "openai-responses", "gpt-5.4", {
      messages: [{ role: "user", content: "hello" }],
      max_completion_tokens: 12,
      reasoning_effort: "high",
    }, true, null, "openai-compatible-responses-demo");

    expect(result.max_output_tokens).toBe(12);
    expect(result.reasoning?.effort).toBe("high");
    expect(result.max_completion_tokens).toBeUndefined();
    expect(result.reasoning_effort).toBeUndefined();
  });
});
