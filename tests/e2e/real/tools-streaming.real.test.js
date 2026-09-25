import { describe, expect, it } from "vitest";
import {
  REAL_TIMEOUT_MS,
  RUN_REAL,
  invokeChat,
  prepareTarget,
  requireChatSuccess,
  textFromJson,
  toolCallFromJson,
} from "./_helpers.js";

const suite = RUN_REAL ? describe : describe.skip;
const TOOL = {
  type: "function",
  function: {
    name: "get_weather",
    description: "Return weather for a city",
    parameters: {
      type: "object",
      properties: {
        city: { type: "string" },
      },
      required: ["city"],
      additionalProperties: false,
    },
  },
};

async function toolTarget(context) {
  const target = await prepareTarget({
    requiredCapabilities: { tools: true },
    modelPredicate: ({ capabilities }) => !capabilities.reasoning,
  });
  if (!target) context.skip("no active credential with a non-reasoning tools-capable chat model");
  return target;
}

function parseArguments(value) {
  expect(typeof value).toBe("string");
  return JSON.parse(value);
}

suite("REAL tools and streaming", () => {
  it("forces a tool call", async (context) => {
    const target = await toolTarget(context);
    const result = await invokeChat(target, {
      stream: false,
      max_tokens: 128,
      tools: [TOOL],
      tool_choice: { type: "function", function: { name: "get_weather" } },
      messages: [{ role: "user", content: "Use get_weather for Paris." }],
    });
    requireChatSuccess(context, target, result);
    const call = toolCallFromJson(result.json);
    expect(call?.function?.name).toBe("get_weather");
    expect(parseArguments(call.function.arguments).city.toLowerCase()).toContain("paris");
  }, REAL_TIMEOUT_MS);

  it("roundtrips a tool result", async (context) => {
    const target = await toolTarget(context);
    const first = await invokeChat(target, {
      stream: false,
      max_tokens: 128,
      tools: [TOOL],
      tool_choice: { type: "function", function: { name: "get_weather" } },
      messages: [{ role: "user", content: "Use get_weather for Paris." }],
    });
    requireChatSuccess(context, target, first);
    const call = toolCallFromJson(first.json);
    expect(call).toBeTruthy();
    const second = await invokeChat(target, {
      stream: false,
      max_tokens: 64,
      tools: [TOOL],
      tool_choice: "auto",
      messages: [
        { role: "user", content: "Use get_weather for Paris." },
        { role: "assistant", content: null, tool_calls: [call] },
        { role: "tool", tool_call_id: call.id, content: '{"temperature_c":18,"condition":"sunny"}' },
        { role: "user", content: "Reply exactly OK_18_SUNNY using the tool result." },
      ],
    });
    requireChatSuccess(context, target, second);
    expect(textFromJson(second.json).toUpperCase()).toContain("OK_18_SUNNY");
  }, REAL_TIMEOUT_MS);

  it("honors tool_choice none", async (context) => {
    const target = await toolTarget(context);
    const result = await invokeChat(target, {
      stream: false,
      max_tokens: 32,
      tools: [TOOL],
      tool_choice: "none",
      messages: [{ role: "user", content: "Reply exactly NO_TOOL and do not call a tool." }],
    });
    requireChatSuccess(context, target, result);
    expect(toolCallFromJson(result.json)).toBeFalsy();
    expect(textFromJson(result.json).toUpperCase()).toContain("NO_TOOL");
  }, REAL_TIMEOUT_MS);

  it("emits a stream terminal event", async (context) => {
    const target = await toolTarget(context);
    const result = await invokeChat(target, {
      stream: true,
      max_tokens: 32,
      messages: [{ role: "user", content: "Reply exactly TERMINATED." }],
    });
    requireChatSuccess(context, target, result);
    expect(result.stream.terminal).toBe(true);
    expect(result.stream.text.toUpperCase()).toContain("TERMINATED");
  }, REAL_TIMEOUT_MS);

  it("keeps stream and non-stream answers equivalent", async (context) => {
    const target = await toolTarget(context);
    const body = {
      max_tokens: 32,
      messages: [{ role: "user", content: "Reply exactly 7391." }],
    };
    const streamed = await invokeChat(target, { ...body, stream: true });
    requireChatSuccess(context, target, streamed);
    const completed = await invokeChat(target, { ...body, stream: false });
    requireChatSuccess(context, target, completed);
    expect(streamed.stream.text.trim()).toBe(textFromJson(completed.json).trim());
  }, REAL_TIMEOUT_MS);

  it("includes a usage frame in a stream", async (context) => {
    const target = await toolTarget(context);
    const result = await invokeChat(target, {
      stream: true,
      max_tokens: 32,
      stream_options: { include_usage: true },
      messages: [{ role: "user", content: "Reply exactly USAGE_OK." }],
    });
    requireChatSuccess(context, target, result);
    expect(result.stream.usage).toBeTruthy();
  }, REAL_TIMEOUT_MS);
});
