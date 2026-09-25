import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  auth: vi.fn(),
  getApiKeys: vi.fn(),
  getApiKeyById: vi.fn(),
  createApiKey: vi.fn(),
  updateApiKey: vi.fn(),
  deleteApiKey: vi.fn(),
  regenerateApiKey: vi.fn(),
  getConsistentMachineId: vi.fn(),
}));

vi.mock("@/lib/auth/adminApiToken", () => ({ hasValidAdminApiToken: mocks.auth }));
vi.mock("@/lib/localDb", () => ({
  getApiKeys: mocks.getApiKeys,
  getApiKeyById: mocks.getApiKeyById,
  createApiKey: mocks.createApiKey,
  updateApiKey: mocks.updateApiKey,
  deleteApiKey: mocks.deleteApiKey,
}));
vi.mock("@/lib/db/index.js", () => ({ regenerateApiKey: mocks.regenerateApiKey }));
vi.mock("@/shared/utils/machineId", () => ({ getConsistentMachineId: mocks.getConsistentMachineId }));

const { GET: list, POST: create } = await import("@/app/api/admin/keys/route.js");
const { GET: read, PUT: update, DELETE: remove } = await import("@/app/api/admin/keys/[id]/route.js");
const { POST: rotatePost, PUT: rotatePut } = await import("@/app/api/admin/keys/[id]/rotate/route.js");

const existing = {
  id: "key-1",
  key: "sk-secret",
  name: "Production",
  ownerRef: "owner-1",
  machineId: "machine-1",
  isActive: true,
  createdAt: "2026-01-01T00:00:00.000Z",
  rateLimitRequests: 10,
  rateLimitWindowSeconds: 60,
  tokenLimitDaily: 100,
  tokenLimitMonthly: 1000,
  allowedModels: "gpt-5",
  blockedModels: "",
  allowedIPs: "10.0.0.0/8",
  blockedIPs: "",
  expiresAt: "2099-01-01T00:00:00.000Z",
  description: "Managed key",
  budgetUsd: 10,
  budgetResetHours: 24,
  budgetWindowStartedAt: "2026-01-01T00:00:00.000Z",
  billingMode: "credits",
};

function request(url, body, authorized = true) {
  const headers = authorized ? { authorization: "Bearer admin" } : {};
  return new Request(url, {
    method: body === undefined ? "GET" : "POST",
    headers: { ...headers, "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
}

let current;

beforeEach(() => {
  vi.clearAllMocks();
  current = { ...existing };
  mocks.auth.mockResolvedValue(true);
  mocks.getApiKeys.mockResolvedValue([existing]);
  mocks.getApiKeyById.mockResolvedValue(existing);
  mocks.getConsistentMachineId.mockResolvedValue("machine-generated");
  mocks.createApiKey.mockImplementation(async (name, machineId, options) => {
    current = {
      ...current,
      ...options,
      name,
      machineId,
      key: options.key || "sk-generated-secret",
    };
    return { ...current };
  });
  mocks.updateApiKey.mockImplementation(async (id, data) => {
    current = { ...current, ...data };
    return { ...current };
  });
  mocks.deleteApiKey.mockResolvedValue(true);
  mocks.regenerateApiKey.mockResolvedValue({ ...existing, key: "sk-rotated-secret" });
});

describe("admin key API authentication and ownership", () => {
  it("rejects requests without a valid admin token", async () => {
    mocks.auth.mockResolvedValue(false);
    const response = await list(request("http://localhost/api/admin/keys?ownerRef=owner-1", undefined, false));
    expect(response.status).toBe(401);
    expect(mocks.getApiKeys).not.toHaveBeenCalled();
  });

  it("requires ownerRef for list, read, update, delete, and rotate", async () => {
    expect((await list(request("http://localhost/api/admin/keys"))).status).toBe(400);
    expect((await read(request("http://localhost/api/admin/keys/key-1"), { params: Promise.resolve({ id: "key-1" }) })).status).toBe(400);
    expect((await update(request("http://localhost/api/admin/keys/key-1", {}), { params: Promise.resolve({ id: "key-1" }) })).status).toBe(400);
    expect((await remove(request("http://localhost/api/admin/keys/key-1"), { params: Promise.resolve({ id: "key-1" }) })).status).toBe(400);
    expect((await rotatePost(request("http://localhost/api/admin/keys/key-1/rotate", {}), { params: Promise.resolve({ id: "key-1" }) })).status).toBe(400);
  });

  it("returns 404 instead of revealing a key owned by another owner", async () => {
    const response = await read(request("http://localhost/api/admin/keys/key-1?ownerRef=owner-2"), { params: Promise.resolve({ id: "key-1" }) });
    expect(response.status).toBe(404);
  });
});

describe("admin key API policy operations", () => {
  it("creates a normalized key and returns its secret once", async () => {
    const response = await create(request("http://localhost/api/admin/keys", {
      ownerRef: " owner-1 ",
      name: " Production ",
      key: " custom-key ",
      description: " Managed ",
      isActive: false,
      rateLimitRequests: 25,
      rateLimitWindowSeconds: 30,
      tokenLimitDaily: 1000,
      tokenLimitMonthly: 2000,
      allowedModels: ["gpt-5", "gpt-5", "claude"],
      blockedModels: "old-model",
      allowedIPs: "10.0.0.0/8",
      blockedIPs: "192.168.0.0/16",
      expiresAt: "2099-01-01T00:00:00Z",
      budgetUsd: 12.5,
      budgetResetHours: 24,
      budgetWindowStartedAt: "2026-01-01T00:00:00Z",
      billingMode: "credits",
      machineId: " machine-2 ",
    }));
    expect(response.status).toBe(201);
    const body = await response.json();
    expect(body.key).toBe("custom-key");
    expect(body.ownerRef).toBe("owner-1");
    expect(body.allowedModels).toBe("gpt-5,claude");
    expect(mocks.createApiKey).toHaveBeenCalledWith("Production", "machine-2", expect.objectContaining({
      ownerRef: "owner-1",
      key: "custom-key",
    }));
    expect(mocks.updateApiKey).toHaveBeenCalledWith("key-1", { isActive: false });
  });

  it("rejects invalid policy values and duplicate custom keys", async () => {
    const invalid = await create(request("http://localhost/api/admin/keys", {
      ownerRef: "owner-1",
      name: "Production",
      key: "has whitespace",
      billingMode: "invalid",
    }));
    expect(invalid.status).toBe(400);
    expect((await invalid.json()).error).toContain("key");

    mocks.createApiKey.mockRejectedValueOnce(Object.assign(new Error("duplicate"), { code: "DUPLICATE_API_KEY" }));
    const duplicate = await create(request("http://localhost/api/admin/keys", { ownerRef: "owner-1", name: "Production", key: "duplicate" }));
    expect(duplicate.status).toBe(409);
  });

  it("lists and reads without exposing the secret", async () => {
    const listed = await list(request("http://localhost/api/admin/keys?ownerRef=owner-1"));
    const listedBody = await listed.json();
    expect(listedBody.keys[0].key).toBeUndefined();
    const readResponse = await read(request("http://localhost/api/admin/keys/key-1?ownerRef=owner-1"), { params: Promise.resolve({ id: "key-1" }) });
    expect((await readResponse.json()).key).toBeUndefined();
  });

  it("updates policy, checks list overlap, and omits the secret", async () => {
    const response = await update(request("http://localhost/api/admin/keys/key-1?ownerRef=owner-1", {
      description: "Updated",
      allowedModels: ["gpt-5", "new-model"],
      blockedModels: "old-model",
    }), { params: Promise.resolve({ id: "key-1" }) });
    expect(response.status).toBe(200);
    expect((await response.json()).key).toBeUndefined();
    expect(mocks.updateApiKey).toHaveBeenCalledWith("key-1", expect.objectContaining({ description: "Updated", allowedModels: "gpt-5,new-model" }));

    const conflict = await update(request("http://localhost/api/admin/keys/key-1?ownerRef=owner-1", {
      allowedModels: "blocked-model",
      blockedModels: ["blocked-model"],
    }), { params: Promise.resolve({ id: "key-1" }) });
    expect(conflict.status).toBe(400);
  });

  it("deletes only an owned key", async () => {
    const response = await remove(request("http://localhost/api/admin/keys/key-1?ownerRef=owner-1"), { params: Promise.resolve({ id: "key-1" }) });
    expect(response.status).toBe(200);
    expect(mocks.deleteApiKey).toHaveBeenCalledWith("key-1");
  });

  it("rotates with POST and PUT and returns the new secret", async () => {
    for (const method of [rotatePost, rotatePut]) {
      const response = await method(request("http://localhost/api/admin/keys/key-1/rotate", { ownerRef: "owner-1" }), { params: Promise.resolve({ id: "key-1" }) });
      expect(response.status).toBe(200);
      expect((await response.json()).key).toBe("sk-rotated-secret");
    }
    expect(mocks.regenerateApiKey).toHaveBeenCalledTimes(2);
  });
});
