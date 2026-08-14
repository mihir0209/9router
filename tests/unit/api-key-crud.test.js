import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

let tempDir;
const originalDataDir = process.env.DATA_DIR;

beforeEach(() => {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "9router-crud-"));
  process.env.DATA_DIR = tempDir;
  delete global._dbAdapter;
  vi.resetModules();
});

afterEach(() => {
  try { global._dbAdapter?.instance?.close?.(); } catch {}
  delete global._dbAdapter;
  if (tempDir) fs.rmSync(tempDir, { recursive: true, force: true });
  if (originalDataDir === undefined) delete process.env.DATA_DIR;
  else process.env.DATA_DIR = originalDataDir;
});

describe("API Keys CRUD - create with options", () => {
  it("creates key with all new fields", async () => {
    const { createApiKey } = await import("@/lib/db/repos/apiKeysRepo.js");
    const { getConsistentMachineId } = await import("@/shared/utils/machineId.js");
    const machineId = await getConsistentMachineId();

    const key = await createApiKey("test-key", machineId, {
      rateLimitRequests: 100,
      rateLimitWindowSeconds: 60,
      tokenLimitDaily: 1000000,
      tokenLimitMonthly: 30000000,
      allowedModels: "gpt-4,claude-3",
      blockedModels: "gpt-3.5",
      allowedIPs: "192.168.1.0/24",
      blockedIPs: "10.0.0.1",
      expiresAt: "2099-12-31T23:59:59Z",
      description: "Production key",
      ownerRef: "workspace-1",
      budgetUsd: 25.5,
      budgetResetHours: 168,
      budgetWindowStartedAt: "2099-01-01T00:00:00Z",
      billingMode: "byok",
      byokProvider: "openai",
      byokCredentialsCiphertext: "encrypted",
      byokCredsEncVersion: 1,
      regenerateVersion: 2,
    });

    expect(key.id).toBeTruthy();
    expect(key.key).toMatch(/^sk-/);
    expect(key.name).toBe("test-key");
    expect(key.rateLimitRequests).toBe(100);
    expect(key.rateLimitWindowSeconds).toBe(60);
    expect(key.tokenLimitDaily).toBe(1000000);
    expect(key.tokenLimitMonthly).toBe(30000000);
    expect(key.allowedModels).toBe("gpt-4,claude-3");
    expect(key.blockedModels).toBe("gpt-3.5");
    expect(key.allowedIPs).toBe("192.168.1.0/24");
    expect(key.blockedIPs).toBe("10.0.0.1");
    expect(key.expiresAt).toBe("2099-12-31T23:59:59Z");
    expect(key.description).toBe("Production key");
    expect(key.ownerRef).toBe("workspace-1");
    expect(key.budgetUsd).toBe(25.5);
    expect(key.budgetResetHours).toBe(168);
    expect(key.budgetWindowStartedAt).toBe("2099-01-01T00:00:00Z");
    expect(key.billingMode).toBe("byok");
    expect(key.byokProvider).toBe("openai");
    expect(key.byokCredentialsCiphertext).toBe("encrypted");
    expect(key.byokCredsEncVersion).toBe(1);
    expect(key.regenerateVersion).toBe(2);
  });

  it("creates key with defaults when no options", async () => {
    const { createApiKey } = await import("@/lib/db/repos/apiKeysRepo.js");
    const { getConsistentMachineId } = await import("@/shared/utils/machineId.js");
    const machineId = await getConsistentMachineId();

    const key = await createApiKey("test-key", machineId);

    expect(key.rateLimitRequests).toBe(0);
    expect(key.rateLimitWindowSeconds).toBe(60);
    expect(key.tokenLimitDaily).toBe(0);
    expect(key.tokenLimitMonthly).toBe(0);
    expect(key.allowedModels).toBe("");
    expect(key.blockedModels).toBe("");
    expect(key.allowedIPs).toBe("");
    expect(key.blockedIPs).toBe("");
    expect(key.expiresAt).toBe("");
    expect(key.description).toBe("");
    expect(key.ownerRef).toBeNull();
    expect(key.budgetUsd).toBeNull();
    expect(key.billingMode).toBe("credits");
    expect(key.regenerateVersion).toBe(0);
  });
});

describe("API Keys CRUD - read", () => {
  it("getApiKeyByKey returns full key record", async () => {
    const { createApiKey, getApiKeyByKey } = await import("@/lib/db/repos/apiKeysRepo.js");
    const { getConsistentMachineId } = await import("@/shared/utils/machineId.js");
    const machineId = await getConsistentMachineId();

    const created = await createApiKey("test-key", machineId, {
      rateLimitRequests: 50,
      description: "test desc",
    });

    const found = await getApiKeyByKey(created.key);
    expect(found).toBeTruthy();
    expect(found.id).toBe(created.id);
    expect(found.rateLimitRequests).toBe(50);
    expect(found.description).toBe("test desc");
  });

  it("getApiKeyByKey returns null for non-existent key", async () => {
    const { getApiKeyByKey } = await import("@/lib/db/repos/apiKeysRepo.js");
    const found = await getApiKeyByKey("sk-nonexistent");
    expect(found).toBeNull();
  });

  it("getApiKeys returns all keys", async () => {
    const { createApiKey, getApiKeys } = await import("@/lib/db/repos/apiKeysRepo.js");
    const { getConsistentMachineId } = await import("@/shared/utils/machineId.js");
    const machineId = await getConsistentMachineId();

    await createApiKey("key-1", machineId);
    await createApiKey("key-2", machineId);

    const keys = await getApiKeys();
    expect(keys.length).toBe(2);
    expect(keys.map(k => k.name)).toContain("key-1");
    expect(keys.map(k => k.name)).toContain("key-2");
  });

  it("getApiKeys filters by ownerRef", async () => {
    const { createApiKey, getApiKeys } = await import("@/lib/db/repos/apiKeysRepo.js");
    const { getConsistentMachineId } = await import("@/shared/utils/machineId.js");
    const machineId = await getConsistentMachineId();

    await createApiKey("key-1", machineId, { ownerRef: "workspace-1" });
    await createApiKey("key-2", machineId, { ownerRef: "workspace-2" });

    const keys = await getApiKeys("workspace-1");
    expect(keys).toHaveLength(1);
    expect(keys[0].ownerRef).toBe("workspace-1");
  });
});

describe("API Keys CRUD - update", () => {
  it("updates individual fields", async () => {
    const { createApiKey, updateApiKey, getApiKeyById } = await import("@/lib/db/repos/apiKeysRepo.js");
    const { getConsistentMachineId } = await import("@/shared/utils/machineId.js");
    const machineId = await getConsistentMachineId();

    const key = await createApiKey("test-key", machineId);

    await updateApiKey(key.id, { rateLimitRequests: 200 });
    const updated = await getApiKeyById(key.id);
    expect(updated.rateLimitRequests).toBe(200);
    expect(updated.name).toBe("test-key");

    await updateApiKey(key.id, { description: "updated desc" });
    const updated2 = await getApiKeyById(key.id);
    expect(updated2.description).toBe("updated desc");
    expect(updated2.rateLimitRequests).toBe(200);
  });

  it("toggles isActive", async () => {
    const { createApiKey, updateApiKey, getApiKeyById } = await import("@/lib/db/repos/apiKeysRepo.js");
    const { getConsistentMachineId } = await import("@/shared/utils/machineId.js");
    const machineId = await getConsistentMachineId();

    const key = await createApiKey("test-key", machineId);
    expect(key.isActive).toBe(true);

    await updateApiKey(key.id, { isActive: false });
    const updated = await getApiKeyById(key.id);
    expect(updated.isActive).toBe(false);
  });

  it("regenerates atomically, invalidates the old key, and bumps the version", async () => {
    const { createApiKey, regenerateApiKey, validateApiKey } = await import("@/lib/db/repos/apiKeysRepo.js");
    const { getConsistentMachineId } = await import("@/shared/utils/machineId.js");
    const key = await createApiKey("test-key", await getConsistentMachineId());

    const regenerated = await regenerateApiKey(key.id);

    expect(regenerated.key).not.toBe(key.key);
    expect(regenerated.regenerateVersion).toBe(1);
    expect(await validateApiKey(key.key)).toBe(false);
    expect(await validateApiKey(regenerated.key)).toBe(true);
  });
});

describe("API Keys CRUD - validate", () => {
  it("validates active key", async () => {
    const { createApiKey, validateApiKey } = await import("@/lib/db/repos/apiKeysRepo.js");
    const { getConsistentMachineId } = await import("@/shared/utils/machineId.js");
    const machineId = await getConsistentMachineId();

    const key = await createApiKey("test-key", machineId);
    const valid = await validateApiKey(key.key);
    expect(valid).toBe(true);
  });

  it("rejects inactive key", async () => {
    const { createApiKey, updateApiKey, validateApiKey } = await import("@/lib/db/repos/apiKeysRepo.js");
    const { getConsistentMachineId } = await import("@/shared/utils/machineId.js");
    const machineId = await getConsistentMachineId();

    const key = await createApiKey("test-key", machineId);
    await updateApiKey(key.id, { isActive: false });

    const valid = await validateApiKey(key.key);
    expect(valid).toBe(false);
  });

  it("rejects expired key", async () => {
    const { createApiKey, validateApiKey } = await import("@/lib/db/repos/apiKeysRepo.js");
    const { getConsistentMachineId } = await import("@/shared/utils/machineId.js");
    const machineId = await getConsistentMachineId();

    const key = await createApiKey("test-key", machineId, {
      expiresAt: "2020-01-01T00:00:00Z",
    });

    const valid = await validateApiKey(key.key);
    expect(valid).toBe(false);
  });

  it("rejects non-existent key", async () => {
    const { validateApiKey } = await import("@/lib/db/repos/apiKeysRepo.js");
    const valid = await validateApiKey("sk-fakekey");
    expect(valid).toBe(false);
  });
});

describe("API Keys CRUD - delete", () => {
  it("deletes key", async () => {
    const { createApiKey, deleteApiKey, getApiKeyById } = await import("@/lib/db/repos/apiKeysRepo.js");
    const { getConsistentMachineId } = await import("@/shared/utils/machineId.js");
    const machineId = await getConsistentMachineId();

    const key = await createApiKey("test-key", machineId);
    const deleted = await deleteApiKey(key.id);
    expect(deleted).toBe(true);

    const found = await getApiKeyById(key.id);
    expect(found).toBeNull();
  });

  it("returns false for non-existent key", async () => {
    const { deleteApiKey } = await import("@/lib/db/repos/apiKeysRepo.js");
    const deleted = await deleteApiKey("non-existent-id");
    expect(deleted).toBe(false);
  });
});
