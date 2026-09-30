import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  buildModelsList: vi.fn(),
  getApiKeyByKey: vi.fn(async (key) =>
    key ? { id: "test-key", key, isActive: true, allowedModels: "", expiresAt: "" } : null
  ),
}));

vi.mock("../../src/app/api/v1/models/route.js", () => ({
  buildModelsList: mocks.buildModelsList,
}));

// /v1/models is authenticated: every catalogue route requires a live key, so
// these requests carry one. Without it the route 401s before reaching the
// lookup, which is the behaviour asserted in models-auth-and-cache.test.js.
vi.mock("../../src/lib/localDb.js", () => ({
  getApiKeyByKey: mocks.getApiKeyByKey,
}));

const { GET } = await import("../../src/app/api/v1/models/[...model]/route.js");

const chatModel = {
  id: "cc/claude-sonnet-5",
  object: "model",
  owned_by: "cc",
  context_length: 1_000_000,
};

function params(model) {
  return { params: Promise.resolve({ model }) };
}

function authedRequest(url) {
  return new Request(url, { headers: { authorization: "Bearer sk-test" } });
}

describe("GET /v1/models/{id}", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("retrieves a provider-prefixed model ID split across URL path segments", async () => {
    mocks.buildModelsList.mockResolvedValue([chatModel]);

    const response = await GET(authedRequest("https://router.test/v1/models/cc/claude-sonnet-5"), params(["cc", "claude-sonnet-5"]));

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual(chatModel);
    expect(mocks.buildModelsList).toHaveBeenCalledWith(["llm"], { allowedModels: null });
  });

  it("also handles a decoded slash in a single catch-all segment", async () => {
    mocks.buildModelsList.mockResolvedValue([chatModel]);

    const response = await GET(authedRequest("https://router.test/v1/models/cc%2Fclaude-sonnet-5"), params(["cc/claude-sonnet-5"]));

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual(chatModel);
  });

  it("keeps capability-list routes unchanged", async () => {
    const imageModel = { id: "image/gpt-image-1", object: "model", owned_by: "image" };
    mocks.buildModelsList.mockResolvedValue([imageModel]);

    const response = await GET(authedRequest("https://router.test/v1/models/image"), params(["image"]));

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ object: "list", data: [imageModel] });
    expect(mocks.buildModelsList).toHaveBeenCalledWith(["image"], { allowedModels: null });
  });

  it("returns an OpenAI-style model_not_found response for an unknown model", async () => {
    mocks.buildModelsList.mockResolvedValue([chatModel]);

    const response = await GET(authedRequest("https://router.test/v1/models/cc/missing-model"), params(["cc", "missing-model"]));
    const body = await response.json();

    expect(response.status).toBe(404);
    expect(body.error).toMatchObject({
      type: "invalid_request_error",
      code: "model_not_found",
    });
  });
});
