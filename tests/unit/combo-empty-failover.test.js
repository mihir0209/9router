import { describe, it, expect, vi } from "vitest";

import { handleComboChat } from "../../open-sse/services/combo.js";
import { deltaHasContent, inspectSSELine, scanChunk, isNoRetryFinish } from "../../open-sse/services/emptyFailover.js";

function sseResponse(lines) {
  const encoder = new TextEncoder();
  return new Response(new ReadableStream({
    start(controller) {
      for (const line of lines) controller.enqueue(encoder.encode(line));
      controller.close();
    },
  }), {
    status: 200,
    headers: { "Content-Type": "text/event-stream" },
  });
}

const ROLE_FRAME = 'data: {"choices":[{"delta":{"role":"assistant"}}]}\n\n';
const DONE = "data: [DONE]\n\n";
const contentFrame = (text) => `data: {"choices":[{"delta":{"content":${JSON.stringify(text)}}}]}\n\n`;
const stopFrame = (reason) => `data: {"choices":[{"delta":{},"finish_reason":${JSON.stringify(reason)}}]}\n\n`;
const okLog = () => ({ info: vi.fn(), warn: vi.fn() });

describe("emptyFailover matchers", () => {
  it("counts text, arrays, tool calls and output_text as content", () => {
    expect(deltaHasContent({ content: "hi" })).toBe(true);
    expect(deltaHasContent({ content: [{ type: "text" }] })).toBe(true);
    expect(deltaHasContent({ tool_calls: [{ id: "1" }] })).toBe(true);
    expect(deltaHasContent({ output_text: "hi" })).toBe(true);
  });

  it("ignores role-only frames, empty content and reasoning", () => {
    expect(deltaHasContent({ role: "assistant" })).toBe(false);
    expect(deltaHasContent({ content: "" })).toBe(false);
    expect(deltaHasContent({ reasoning_content: "thinking…" })).toBe(false);
    expect(deltaHasContent(null)).toBe(false);
  });

  it("inspectSSELine reports content and finish reasons", () => {
    expect(inspectSSELine(contentFrame("hi")).content).toBe(true);
    expect(inspectSSELine(ROLE_FRAME).content).toBe(false);
    expect(inspectSSELine(DONE)).toEqual({ content: false, finishReason: null });
    expect(inspectSSELine(stopFrame("stop")).finishReason).toBe("stop");
  });

  it("only content_filter is exempt from retry", () => {
    expect(isNoRetryFinish("content_filter")).toBe(true);
    expect(isNoRetryFinish("CONTENT_FILTER")).toBe(true);
    expect(isNoRetryFinish("stop")).toBe(false);
    expect(isNoRetryFinish(null)).toBe(false);
  });

  it("scanChunk holds fragments across chunks", () => {
    const half = contentFrame("hello").slice(0, 20);
    const rest = contentFrame("hello").slice(20);
    const first = scanChunk(half);
    expect(first.sawContent).toBe(false);
    expect(first.pending.length).toBeGreaterThan(0);
    const second = scanChunk(rest, first);
    expect(second.sawContent).toBe(true);
  });
});

describe("combo empty-stream failover (upstream #4364, adapted)", () => {
  it("falls through when a member streams role-only frames then ends", async () => {
    const handleSingleModel = vi.fn()
      .mockResolvedValueOnce(sseResponse([ROLE_FRAME, stopFrame("stop"), DONE]))
      .mockResolvedValueOnce(sseResponse([contentFrame("real answer"), DONE]));

    const result = await handleComboChat({
      body: { stream: true },
      models: ["webapi/model-a", "backup/model-b"],
      handleSingleModel,
      log: okLog(),
    });

    expect(handleSingleModel).toHaveBeenCalledTimes(2);
    expect(await result.text()).toContain("real answer");
  });

  it("falls through on reasoning-only then end (think-then-abort)", async () => {
    const handleSingleModel = vi.fn()
      .mockResolvedValueOnce(sseResponse([
        'data: {"choices":[{"delta":{"reasoning_content":"hmm"}}]}\n\n',
        stopFrame("stop"),
        DONE,
      ]))
      .mockResolvedValueOnce(sseResponse([contentFrame("fallback answer"), DONE]));

    const result = await handleComboChat({
      body: { stream: true },
      models: ["webapi/model-a", "backup/model-b"],
      handleSingleModel,
      log: okLog(),
    });

    expect(handleSingleModel).toHaveBeenCalledTimes(2);
    expect(await result.text()).toContain("fallback answer");
  });

  it("commits a content_filter empty without retrying", async () => {
    const handleSingleModel = vi.fn()
      .mockResolvedValueOnce(sseResponse([ROLE_FRAME, stopFrame("content_filter"), DONE]));

    const result = await handleComboChat({
      body: { stream: true },
      models: ["webapi/model-a", "backup/model-b"],
      handleSingleModel,
      log: okLog(),
    });

    expect(handleSingleModel).toHaveBeenCalledTimes(1);
    expect(result.ok).toBe(true);
  });

  it("commits tool calls as content", async () => {
    const handleSingleModel = vi.fn()
      .mockResolvedValueOnce(sseResponse([
        'data: {"choices":[{"delta":{"tool_calls":[{"id":"c1","function":{"name":"read"}}]}}]}\n\n',
        DONE,
      ]));

    const result = await handleComboChat({
      body: { stream: true },
      models: ["webapi/model-a"],
      handleSingleModel,
      log: okLog(),
    });

    expect(handleSingleModel).toHaveBeenCalledTimes(1);
    expect(await result.text()).toContain("tool_calls");
  });

  it("returns the last candidate as-is when every member streams empty", async () => {
    const handleSingleModel = vi.fn()
      .mockResolvedValueOnce(sseResponse([ROLE_FRAME, stopFrame("stop"), DONE]))
      .mockResolvedValueOnce(sseResponse([stopFrame("length"), DONE]));
    const log = okLog();

    const result = await handleComboChat({
      body: { stream: true },
      models: ["webapi/model-a", "backup/model-b"],
      handleSingleModel,
      log,
    });

    expect(handleSingleModel).toHaveBeenCalledTimes(2);
    expect(result.ok).toBe(true);
    expect(log.warn).toHaveBeenCalledWith("COMBO", expect.stringContaining("last candidate"));
  });
});
