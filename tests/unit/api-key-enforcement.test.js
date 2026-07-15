import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

let tempDir;
const originalDataDir = process.env.DATA_DIR;

function makeRequest(headers = {}) {
  return {
    headers: {
      get: (name) => headers[name.toLowerCase()] || null,
    },
  };
}

beforeEach(() => {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "9router-enforcement-"));
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

describe("API Key Enforcement - rate limiting", () => {
  it("allows requests within rate limit", async () => {
    const { createApiKey } = await import("@/lib/db/repos/apiKeysRepo.js");
    const { getConsistentMachineId } = await import("@/shared/utils/machineId.js");
    const machineId = await getConsistentMachineId();
    const key = await createApiKey("test-key", machineId, {
      rateLimitRequests: 5,
      rateLimitWindowSeconds: 60,
    });

    const { enforceApiKeyRestrictions } = await import("@/sse/services/apiKeyEnforcement.js");
    const request = makeRequest();
    const body = { model: "gpt-4" };

    const result = await enforceApiKeyRestrictions(key.key, request, body);
    expect(result.allowed).toBe(true);
  });

  it("blocks when rate limit exceeded", async () => {
    const { createApiKey } = await import("@/lib/db/repos/apiKeysRepo.js");
    const { getConsistentMachineId } = await import("@/shared/utils/machineId.js");
    const machineId = await getConsistentMachineId();
    const key = await createApiKey("test-key", machineId, {
      rateLimitRequests: 2,
      rateLimitWindowSeconds: 60,
    });

    const { enforceApiKeyRestrictions } = await import("@/sse/services/apiKeyEnforcement.js");
    const request = makeRequest();
    const body = { model: "gpt-4" };

    const r1 = await enforceApiKeyRestrictions(key.key, request, body);
    expect(r1.allowed).toBe(true);
    const r2 = await enforceApiKeyRestrictions(key.key, request, body);
    expect(r2.allowed).toBe(true);

    const r3 = await enforceApiKeyRestrictions(key.key, request, body);
    expect(r3.allowed).toBe(false);
    expect(r3.status).toBe(429);
    expect(r3.retryAfter).toBeGreaterThan(0);
  });

  it("allows unlimited requests when rateLimitRequests is 0", async () => {
    const { createApiKey } = await import("@/lib/db/repos/apiKeysRepo.js");
    const { getConsistentMachineId } = await import("@/shared/utils/machineId.js");
    const machineId = await getConsistentMachineId();
    const key = await createApiKey("test-key", machineId, {
      rateLimitRequests: 0,
      rateLimitWindowSeconds: 60,
    });

    const { enforceApiKeyRestrictions } = await import("@/sse/services/apiKeyEnforcement.js");
    const request = makeRequest();
    const body = { model: "gpt-4" };

    for (let i = 0; i < 50; i++) {
      const result = await enforceApiKeyRestrictions(key.key, request, body);
      expect(result.allowed).toBe(true);
    }
  });
});

describe("API Key Enforcement - IP blocking", () => {
  it("blocks IPs in blockedIPs list", async () => {
    const { createApiKey } = await import("@/lib/db/repos/apiKeysRepo.js");
    const { getConsistentMachineId } = await import("@/shared/utils/machineId.js");
    const machineId = await getConsistentMachineId();
    const key = await createApiKey("test-key", machineId, {
      blockedIPs: "192.168.1.100,10.0.0.5",
    });

    const { enforceApiKeyRestrictions } = await import("@/sse/services/apiKeyEnforcement.js");
    const request = makeRequest({ "x-9r-real-ip": "192.168.1.100" });
    const body = { model: "gpt-4" };

    const result = await enforceApiKeyRestrictions(key.key, request, body);
    expect(result.allowed).toBe(false);
    expect(result.status).toBe(403);
    expect(result.error).toContain("blocked");
  });

  it("allows IPs not in blockedIPs list", async () => {
    const { createApiKey } = await import("@/lib/db/repos/apiKeysRepo.js");
    const { getConsistentMachineId } = await import("@/shared/utils/machineId.js");
    const machineId = await getConsistentMachineId();
    const key = await createApiKey("test-key", machineId, {
      blockedIPs: "192.168.1.100,10.0.0.5",
    });

    const { enforceApiKeyRestrictions } = await import("@/sse/services/apiKeyEnforcement.js");
    const request = makeRequest({ "x-9r-real-ip": "172.16.0.1" });
    const body = { model: "gpt-4" };

    const result = await enforceApiKeyRestrictions(key.key, request, body);
    expect(result.allowed).toBe(true);
  });

  it("allows only IPs in allowedIPs list", async () => {
    const { createApiKey } = await import("@/lib/db/repos/apiKeysRepo.js");
    const { getConsistentMachineId } = await import("@/shared/utils/machineId.js");
    const machineId = await getConsistentMachineId();
    const key = await createApiKey("test-key", machineId, {
      allowedIPs: "192.168.1.0/24",
    });

    const { enforceApiKeyRestrictions } = await import("@/sse/services/apiKeyEnforcement.js");

    const r1 = await enforceApiKeyRestrictions(key.key, makeRequest({ "x-9r-real-ip": "192.168.1.50" }), { model: "gpt-4" });
    expect(r1.allowed).toBe(true);

    const r2 = await enforceApiKeyRestrictions(key.key, makeRequest({ "x-9r-real-ip": "10.0.0.1" }), { model: "gpt-4" });
    expect(r2.allowed).toBe(false);
    expect(r2.status).toBe(403);
  });

  it("supports CIDR notation", async () => {
    const { createApiKey } = await import("@/lib/db/repos/apiKeysRepo.js");
    const { getConsistentMachineId } = await import("@/shared/utils/machineId.js");
    const machineId = await getConsistentMachineId();
    const key = await createApiKey("test-key", machineId, {
      blockedIPs: "10.0.0.0/8",
    });

    const { enforceApiKeyRestrictions } = await import("@/sse/services/apiKeyEnforcement.js");

    const r1 = await enforceApiKeyRestrictions(key.key, makeRequest({ "x-9r-real-ip": "10.1.2.3" }), { model: "gpt-4" });
    expect(r1.allowed).toBe(false);

    const r2 = await enforceApiKeyRestrictions(key.key, makeRequest({ "x-9r-real-ip": "192.168.1.1" }), { model: "gpt-4" });
    expect(r2.allowed).toBe(true);
  });
});

describe("API Key Enforcement - model restrictions", () => {
  it("blocks models not in allowedModels", async () => {
    const { createApiKey } = await import("@/lib/db/repos/apiKeysRepo.js");
    const { getConsistentMachineId } = await import("@/shared/utils/machineId.js");
    const machineId = await getConsistentMachineId();
    const key = await createApiKey("test-key", machineId, {
      allowedModels: "gpt-4,gpt-3.5-turbo",
    });

    const { enforceApiKeyRestrictions } = await import("@/sse/services/apiKeyEnforcement.js");
    const request = makeRequest();

    const r1 = await enforceApiKeyRestrictions(key.key, request, { model: "gpt-4" });
    expect(r1.allowed).toBe(true);

    const r2 = await enforceApiKeyRestrictions(key.key, request, { model: "claude-3-opus" });
    expect(r2.allowed).toBe(false);
    expect(r2.status).toBe(403);
    expect(r2.error).toContain("not allowed");
  });

  it("allows all models when allowedModels contains *", async () => {
    const { createApiKey } = await import("@/lib/db/repos/apiKeysRepo.js");
    const { getConsistentMachineId } = await import("@/shared/utils/machineId.js");
    const machineId = await getConsistentMachineId();
    const key = await createApiKey("test-key", machineId, {
      allowedModels: "*",
    });

    const { enforceApiKeyRestrictions } = await import("@/sse/services/apiKeyEnforcement.js");
    const request = makeRequest();

    const r = await enforceApiKeyRestrictions(key.key, request, { model: "any-model" });
    expect(r.allowed).toBe(true);
  });

  it("blocks models in blockedModels", async () => {
    const { createApiKey } = await import("@/lib/db/repos/apiKeysRepo.js");
    const { getConsistentMachineId } = await import("@/shared/utils/machineId.js");
    const machineId = await getConsistentMachineId();
    const key = await createApiKey("test-key", machineId, {
      blockedModels: "gpt-4,gpt-4-turbo",
    });

    const { enforceApiKeyRestrictions } = await import("@/sse/services/apiKeyEnforcement.js");
    const request = makeRequest();

    const r1 = await enforceApiKeyRestrictions(key.key, request, { model: "gpt-4" });
    expect(r1.allowed).toBe(false);
    expect(r1.error).toContain("blocked");

    const r2 = await enforceApiKeyRestrictions(key.key, request, { model: "gpt-3.5-turbo" });
    expect(r2.allowed).toBe(true);
  });
});

describe("API Key Enforcement - expiry", () => {
  it("rejects expired keys", async () => {
    const { createApiKey } = await import("@/lib/db/repos/apiKeysRepo.js");
    const { getConsistentMachineId } = await import("@/shared/utils/machineId.js");
    const machineId = await getConsistentMachineId();
    const key = await createApiKey("test-key", machineId, {
      expiresAt: "2020-01-01T00:00:00Z",
    });

    const { enforceApiKeyRestrictions } = await import("@/sse/services/apiKeyEnforcement.js");
    const result = await enforceApiKeyRestrictions(key.key, makeRequest(), { model: "gpt-4" });
    expect(result.allowed).toBe(false);
    expect(result.status).toBe(401);
    expect(result.error).toContain("expired");
  });

  it("allows non-expired keys", async () => {
    const { createApiKey } = await import("@/lib/db/repos/apiKeysRepo.js");
    const { getConsistentMachineId } = await import("@/shared/utils/machineId.js");
    const machineId = await getConsistentMachineId();
    const key = await createApiKey("test-key", machineId, {
      expiresAt: "2099-12-31T23:59:59Z",
    });

    const { enforceApiKeyRestrictions } = await import("@/sse/services/apiKeyEnforcement.js");
    const result = await enforceApiKeyRestrictions(key.key, makeRequest(), { model: "gpt-4" });
    expect(result.allowed).toBe(true);
  });

  it("allows keys with no expiry set", async () => {
    const { createApiKey } = await import("@/lib/db/repos/apiKeysRepo.js");
    const { getConsistentMachineId } = await import("@/shared/utils/machineId.js");
    const machineId = await getConsistentMachineId();
    const key = await createApiKey("test-key", machineId, {});

    const { enforceApiKeyRestrictions } = await import("@/sse/services/apiKeyEnforcement.js");
    const result = await enforceApiKeyRestrictions(key.key, makeRequest(), { model: "gpt-4" });
    expect(result.allowed).toBe(true);
  });
});

describe("API Key Enforcement - disabled keys", () => {
  it("rejects disabled keys", async () => {
    const { createApiKey, updateApiKey } = await import("@/lib/db/repos/apiKeysRepo.js");
    const { getConsistentMachineId } = await import("@/shared/utils/machineId.js");
    const machineId = await getConsistentMachineId();
    const key = await createApiKey("test-key", machineId);
    await updateApiKey(key.id, { isActive: false });

    const { enforceApiKeyRestrictions } = await import("@/sse/services/apiKeyEnforcement.js");
    const result = await enforceApiKeyRestrictions(key.key, makeRequest(), { model: "gpt-4" });
    expect(result.allowed).toBe(false);
    expect(result.status).toBe(401);
    expect(result.error).toContain("disabled");
  });
});

describe("API Key Enforcement - invalid key", () => {
  it("rejects non-existent key", async () => {
    const { enforceApiKeyRestrictions } = await import("@/sse/services/apiKeyEnforcement.js");
    const result = await enforceApiKeyRestrictions("sk-fakekey123", makeRequest(), { model: "gpt-4" });
    expect(result.allowed).toBe(false);
    expect(result.status).toBe(401);
  });

  it("allows null key (no key provided)", async () => {
    const { enforceApiKeyRestrictions } = await import("@/sse/services/apiKeyEnforcement.js");
    const result = await enforceApiKeyRestrictions(null, makeRequest(), { model: "gpt-4" });
    expect(result.allowed).toBe(true);
  });
});
