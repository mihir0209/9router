import { beforeEach, describe, it, expect, vi } from "vitest";

import { handleComboChat } from "../../open-sse/services/combo.js";
import { clearRetryPlugins, registerRetryPlugin } from "../../open-sse/retry/index.js";

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

describe("combo streaming fallback", () => {
  beforeEach(() => {
    clearRetryPlugins();
    registerRetryPlugin("webapi", {
      errorRules: [
        { text: "upstream service temporarily unavailable", retryHttp: true, attempts: 2, delayMs: 0 },
      ],
    });
  });

  it("retries the same model, then falls through when a streaming model emits an error before content", async () => {
    const handleSingleModel = vi.fn()
      .mockResolvedValueOnce(sseResponse([
        'data: {"error":{"message":"Upstream service temporarily unavailable"}}\n\n',
      ]))
      .mockResolvedValueOnce(sseResponse([
        'data: {"error":{"message":"Upstream service temporarily unavailable"}}\n\n',
      ]))
      .mockResolvedValueOnce(sseResponse([
        'data: {"error":{"message":"Upstream service temporarily unavailable"}}\n\n',
      ]))
      .mockResolvedValueOnce(new Response(JSON.stringify({ ok: true }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      }));

    const result = await handleComboChat({
      body: { stream: true },
      models: ["webapi/model-a", "backup/model-b"],
      handleSingleModel,
      log: { info: vi.fn(), warn: vi.fn() },
    });

    expect(handleSingleModel).toHaveBeenCalledTimes(4);
    expect(handleSingleModel.mock.calls[0][1]).toBe("webapi/model-a");
    expect(handleSingleModel.mock.calls[1][1]).toBe("webapi/model-a");
    expect(handleSingleModel.mock.calls[2][1]).toBe("webapi/model-a");
    expect(handleSingleModel.mock.calls[3][1]).toBe("backup/model-b");
    expect(result.ok).toBe(true);
    expect(await result.json()).toEqual({ ok: true });
  });

  it("does not fall through once the stream contains useful content", async () => {
    const first = sseResponse([
      'data: {"choices":[{"delta":{"content":"hello"}}]}\n\n',
      'data: [DONE]\n\n',
    ]);
    const handleSingleModel = vi.fn().mockResolvedValueOnce(first);

    const result = await handleComboChat({
      body: { stream: true },
      models: ["webapi/model-a", "backup/model-b"],
      handleSingleModel,
      log: { info: vi.fn(), warn: vi.fn() },
    });

    expect(handleSingleModel).toHaveBeenCalledTimes(1);
    expect(result.ok).toBe(true);
    expect(result.headers.get("content-type")).toContain("text/event-stream");
    expect(await result.text()).toContain("hello");
  });

  it("returns 429 when retryable early stream errors exhaust every combo model", async () => {
    const handleSingleModel = vi.fn()
      .mockResolvedValueOnce(sseResponse([
        'data: {"error":{"message":"Upstream service temporarily unavailable"}}\n\n',
      ]))
      .mockResolvedValueOnce(sseResponse([
        'data: {"error":{"message":"Upstream service temporarily unavailable"}}\n\n',
      ]))
      .mockResolvedValueOnce(sseResponse([
        'data: {"error":{"message":"Upstream service temporarily unavailable"}}\n\n',
      ]));

    const result = await handleComboChat({
      body: { stream: true },
      models: ["webapi/model-a"],
      handleSingleModel,
      log: { info: vi.fn(), warn: vi.fn() },
    });

    expect(handleSingleModel).toHaveBeenCalledTimes(3);
    expect(result.status).toBe(429);
    expect(await result.json()).toEqual({ error: { message: "Upstream service temporarily unavailable" } });
  });
});
