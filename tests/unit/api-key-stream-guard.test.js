import { beforeEach, describe, expect, it, vi } from "vitest";

const getApiKeyById = vi.fn();
const checkBudget = vi.fn();
vi.mock("@/lib/db/index.js", () => ({ getApiKeyById }));
vi.mock("@/lib/budgetService.js", () => ({ checkBudget }));

async function read(response) {
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let output = "";
  while (true) {
    const { done, value } = await reader.read();
    if (done) return output + decoder.decode();
    output += decoder.decode(value, { stream: true });
  }
}

describe("API key stream guard", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    checkBudget.mockResolvedValue({ exceeded: false });
  });

  it("stops output at the next transformed chunk after suspension", async () => {
    getApiKeyById
      .mockResolvedValueOnce({ id: "key-1", key: "sk-old", isActive: true })
      .mockResolvedValueOnce({ id: "key-1", key: "sk-old", isActive: false });
    const encoder = new TextEncoder();
    const source = new ReadableStream({
      start(controller) {
        controller.enqueue(encoder.encode("data: first\n\n"));
        controller.enqueue(encoder.encode("data: second\n\n"));
        controller.close();
      },
    });
    const { guardApiKeyResponse } = await import("@/sse/services/apiKeyStreamGuard.js");

    const output = await read(guardApiKeyResponse(new Response(source), {
      id: "key-1",
      key: "sk-old",
    }));

    expect(output).toBe("data: first\n\n");
    expect(getApiKeyById).toHaveBeenCalledTimes(2);
  });

  it("also stops a stream when its key has been regenerated", async () => {
    getApiKeyById.mockResolvedValue({ id: "key-1", key: "sk-new", isActive: true });
    const { guardApiKeyResponse } = await import("@/sse/services/apiKeyStreamGuard.js");

    const output = await read(guardApiKeyResponse(new Response("data: secret\n\n"), {
      id: "key-1",
      key: "sk-old",
    }));

    expect(output).toBe("");
  });

  it("terminates an SSE stream with a budget error event", async () => {
    getApiKeyById.mockResolvedValue({ id: "key-1", key: "sk-old", isActive: true });
    checkBudget.mockResolvedValue({ exceeded: true });
    const { guardApiKeyResponse } = await import("@/sse/services/apiKeyStreamGuard.js");

    const output = await read(guardApiKeyResponse(new Response("data: secret\n\n", {
      headers: { "content-type": "text/event-stream" },
    }), { id: "key-1", key: "sk-old" }));

    expect(output).toContain('\"finish_reason\":\"error\"');
    expect(output).toContain('\"type\":\"budget_exceeded\"');
    expect(output).not.toContain("secret");
  });
});
