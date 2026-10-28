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
  findForcePairMismatch: vi.fn(),
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
vi.mock("@/lib/apiKeys/forcePair", () => ({ findForcePairMismatch: mocks.findForcePairMismatch }));
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
  mocks.findForcePairMismatch.mockResolvedValue(null);
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

describe("POST /api/keys pin pair and prefix normalisation", () => {
  it("rejects a bare forceModel covered by a provider-prefixed blocked entry", async () => {
    const res = await create(post({ name: "bad", forceModel: "gpt-4o", blockedModels: "openai/gpt-4o" }));
    expect(res.status).toBe(400);
    expect((await res.json()).error).toBe("forceModel must not be in blockedModels");
  });

  it("rejects a forceModel/forceProvider pair that does not match", async () => {
    mocks.findForcePairMismatch.mockResolvedValue("forceModel 'openai/gpt-4o' routes to provider 'openai', which does not match forceProvider 'anthropic'");
    const res = await create(post({ name: "bad", forceProvider: "anthropic", forceModel: "openai/gpt-4o" }));
    expect(res.status).toBe(400);
    expect((await res.json()).error).toMatch(/does not match forceProvider 'anthropic'/);
    expect(mocks.findForcePairMismatch).toHaveBeenCalledWith(expect.objectContaining({ forceProvider: "anthropic", forceModel: "openai/gpt-4o" }));
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

  it("rejects changing the pair to a mismatching one", async () => {
    mocks.getApiKeyById.mockResolvedValue({ ...existing, forceProvider: "openai", forceModel: "openai/gpt-4o" });
    mocks.findForcePairMismatch.mockResolvedValue("mismatch");
    const res = await update(put({ forceProvider: "anthropic" }), params);
    expect(res.status).toBe(400);
    expect((await res.json()).error).toBe("mismatch");
    expect(mocks.findForcePairMismatch).toHaveBeenCalledWith({ forceProvider: "anthropic", forceModel: "openai/gpt-4o" });
    expect(mocks.updateApiKey).not.toHaveBeenCalled();
  });

  it("does not re-check an unchanged (stale) pair, so the key stays editable", async () => {
    mocks.getApiKeyById.mockResolvedValue({ ...existing, forceProvider: "anthropic", forceModel: "openai/gpt-4o" });
    mocks.findForcePairMismatch.mockResolvedValue("mismatch");
    const res = await update(put({ forceProvider: "anthropic", blockedModels: "other" }), params);
    expect(res.status).toBe(200);
    expect(mocks.findForcePairMismatch).not.toHaveBeenCalled();
  });

  it("lets the user clear a pin that already conflicts with the stored blocked list", async () => {
    mocks.getApiKeyById.mockResolvedValue({ ...existing, forceModel: "openai/gpt-4o", blockedModels: "openai/gpt-4o" });
    const res = await update(put({ forceModel: "" }), params);
    expect(res.status).toBe(200);
    expect(mocks.updateApiKey).toHaveBeenCalledWith("key-1", { forceModel: "" });
  });

  it("lets the user edit a blocked list while a pre-existing conflict remains", async () => {
    mocks.getApiKeyById.mockResolvedValue({ ...existing, forceModel: "openai/gpt-4o", blockedModels: "openai/gpt-4o" });
    const res = await update(put({ blockedModels: "openai/gpt-4o,x" }), params);
    expect(res.status).toBe(200);
  });

  it("still rejects a different conflicting pin on a key that already has a conflict", async () => {
    mocks.getApiKeyById.mockResolvedValue({ ...existing, forceModel: "openai/gpt-4o", blockedModels: "openai/gpt-4o,openai/o1" });
    const res = await update(put({ forceModel: "openai/o1" }), params);
    expect(res.status).toBe(400);
  });

  it("does not run pin validation for unrelated updates", async () => {
    mocks.getApiKeyById.mockResolvedValue({ ...existing, forceModel: "openai/gpt-4o", blockedModels: "openai/gpt-4o" });
    const res = await update(put({ description: "just a note" }), params);
    expect(res.status).toBe(200);
    expect(mocks.updateApiKey).toHaveBeenCalledWith("key-1", { description: "just a note" });
  });
});
