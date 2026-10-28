import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  auth: vi.fn(),
  getApiKeys: vi.fn(),
  getApiKeyById: vi.fn(),
  getApiKeyByKey: vi.fn(),
  createApiKey: vi.fn(),
  updateApiKey: vi.fn(),
  deleteApiKey: vi.fn(),
  getConsistentMachineId: vi.fn(),
  getBudgetState: vi.fn(),
}));

vi.mock("@/lib/auth/adminApiToken", () => ({ hasValidAdminApiToken: mocks.auth }));
vi.mock("@/lib/localDb", () => ({
  getApiKeys: mocks.getApiKeys,
  getApiKeyById: mocks.getApiKeyById,
  getApiKeyByKey: mocks.getApiKeyByKey,
  createApiKey: mocks.createApiKey,
  updateApiKey: mocks.updateApiKey,
  deleteApiKey: mocks.deleteApiKey,
}));
vi.mock("@/shared/utils/machineId", () => ({ getConsistentMachineId: mocks.getConsistentMachineId }));
vi.mock("@/lib/budgetService.js", () => ({ getBudgetState: mocks.getBudgetState }));

const { POST: create } = await import("@/app/api/keys/route.js");
const { PUT: update } = await import("@/app/api/keys/[id]/route.js");

const existing = {
  id: "key-1",
  key: "sk-secret",
  name: "Dashboard key",
  isActive: true,
  forceProvider: "",
  forceModel: "",
  blockedModels: "",
  blockedProviders: "",
};

function post(body) {
  return new Request("http://localhost/api/keys", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}

function put(body) {
  return new Request("http://localhost/api/keys/key-1", {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}

const params = { params: Promise.resolve({ id: "key-1" }) };

beforeEach(() => {
  vi.clearAllMocks();
  mocks.auth.mockResolvedValue(false);
  mocks.getConsistentMachineId.mockResolvedValue("machine-1");
  mocks.getApiKeyByKey.mockResolvedValue(null);
  mocks.getApiKeyById.mockResolvedValue({ ...existing });
  mocks.createApiKey.mockImplementation(async (name, machineId, options) => ({ ...existing, name, machineId, ...options }));
  mocks.updateApiKey.mockImplementation(async (id, data) => ({ ...existing, ...data }));
});

describe("POST /api/keys pins", () => {
  it("forwards trimmed pins to createApiKey", async () => {
    const res = await create(post({ name: "pinned", forceProvider: " openai ", forceModel: " openai/gpt-4o " }));
    expect(res.status).toBe(201);
    const options = mocks.createApiKey.mock.calls[0][2];
    expect(options.forceProvider).toBe("openai");
    expect(options.forceModel).toBe("openai/gpt-4o");
    const body = await res.json();
    expect(body.forceModel).toBe("openai/gpt-4o");
  });

  it("accepts empty pins", async () => {
    const res = await create(post({ name: "plain", forceProvider: "", forceModel: "" }));
    expect(res.status).toBe(201);
    expect(mocks.createApiKey.mock.calls[0][2]).toMatchObject({ forceProvider: "", forceModel: "" });
  });

  it("rejects non-string pins", async () => {
    for (const body of [{ forceModel: 7 }, { forceProvider: { a: 1 } }, { forceModel: ["a"] }]) {
      const res = await create(post({ name: "bad", ...body }));
      expect(res.status).toBe(400);
    }
    expect(mocks.createApiKey).not.toHaveBeenCalled();
  });

  it("rejects a forceModel that is blocked", async () => {
    const res = await create(post({ name: "bad", forceModel: "openai/gpt-4o", blockedModels: ["openai/gpt-4o"] }));
    expect(res.status).toBe(400);
    expect((await res.json()).error).toBe("forceModel must not be in blockedModels");
    expect(mocks.createApiKey).not.toHaveBeenCalled();
  });

  it("rejects a forceProvider that is blocked", async () => {
    const res = await create(post({ name: "bad", forceProvider: "openai", blockedProviders: "openai" }));
    expect(res.status).toBe(400);
    expect((await res.json()).error).toBe("forceProvider must not be in blockedProviders");
    expect(mocks.createApiKey).not.toHaveBeenCalled();
  });
});

describe("PUT /api/keys/[id] pins", () => {
  it("trims and forwards pins to updateApiKey", async () => {
    const res = await update(put({ forceProvider: " openai ", forceModel: " openai/gpt-4o " }), params);
    expect(res.status).toBe(200);
    expect(mocks.updateApiKey).toHaveBeenCalledWith("key-1", { forceProvider: "openai", forceModel: "openai/gpt-4o" });
  });

  it("empty string clears a pin", async () => {
    mocks.getApiKeyById.mockResolvedValue({ ...existing, forceProvider: "openai", forceModel: "openai/gpt-4o" });
    const res = await update(put({ forceProvider: "", forceModel: "" }), params);
    expect(res.status).toBe(200);
    expect(mocks.updateApiKey).toHaveBeenCalledWith("key-1", { forceProvider: "", forceModel: "" });
  });

  it("rejects non-string pins", async () => {
    for (const body of [{ forceModel: 7 }, { forceProvider: false }, { forceModel: {} }]) {
      const res = await update(put(body), params);
      expect(res.status).toBe(400);
    }
    expect(mocks.updateApiKey).not.toHaveBeenCalled();
  });

  it("rejects a forceModel in the supplied blockedModels", async () => {
    const res = await update(put({ forceModel: "openai/gpt-4o", blockedModels: "openai/gpt-4o" }), params);
    expect(res.status).toBe(400);
    expect((await res.json()).error).toBe("forceModel must not be in blockedModels");
    expect(mocks.updateApiKey).not.toHaveBeenCalled();
  });

  it("rejects a forceModel in the stored blockedModels", async () => {
    mocks.getApiKeyById.mockResolvedValue({ ...existing, blockedModels: "openai/gpt-4o" });
    const res = await update(put({ forceModel: "openai/gpt-4o" }), params);
    expect(res.status).toBe(400);
    expect(mocks.updateApiKey).not.toHaveBeenCalled();
  });

  it("rejects a forceProvider in the stored or supplied blockedProviders", async () => {
    mocks.getApiKeyById.mockResolvedValue({ ...existing, blockedProviders: "openai" });
    let res = await update(put({ forceProvider: "openai" }), params);
    expect(res.status).toBe(400);
    expect((await res.json()).error).toBe("forceProvider must not be in blockedProviders");

    mocks.getApiKeyById.mockResolvedValue({ ...existing });
    res = await update(put({ forceProvider: "openai", blockedProviders: ["openai"] }), params);
    expect(res.status).toBe(400);
    expect(mocks.updateApiKey).not.toHaveBeenCalled();
  });

  it("rejects a blocked list that would cover the stored pin", async () => {
    mocks.getApiKeyById.mockResolvedValue({ ...existing, forceModel: "openai/gpt-4o" });
    const res = await update(put({ blockedModels: "openai/gpt-4o" }), params);
    expect(res.status).toBe(400);
  });

  it("does not run pin validation for unrelated updates", async () => {
    mocks.getApiKeyById.mockResolvedValue({ ...existing, forceModel: "openai/gpt-4o", blockedModels: "openai/gpt-4o" });
    const res = await update(put({ description: "just a note" }), params);
    expect(res.status).toBe(200);
    expect(mocks.updateApiKey).toHaveBeenCalledWith("key-1", { description: "just a note" });
  });
});
