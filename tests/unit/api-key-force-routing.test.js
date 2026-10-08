import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

let tempDir;
const originalDataDir = process.env.DATA_DIR;

function makeRequest(headers = {}) {
  return {
    url: "http://localhost/v1/chat/completions",
    headers: {
      get: (name) => headers[name.toLowerCase()] || null,
    },
  };
}

async function makeKey(options = {}) {
  const { createApiKey } = await import("@/lib/db/repos/apiKeysRepo.js");
  const { getConsistentMachineId } = await import("@/shared/utils/machineId.js");
  const machineId = await getConsistentMachineId();
  return createApiKey(`force-test-${Date.now()}-${Math.random().toString(36).slice(2)}`, machineId, options);
}

beforeEach(() => {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "9router-force-"));
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

describe("API Key force routing (upstream #4541, adapted)", () => {
  it("rewrites the request to forceModel and reports routedModel", async () => {
    const key = await makeKey({ forceModel: "openai/gpt-4o" });
    const { enforceApiKeyRestrictions } = await import("@/sse/services/apiKeyEnforcement.js");
    const body = { model: "anthropic/claude-opus" };
    const result = await enforceApiKeyRestrictions(key.key, makeRequest(), body);
    expect(result.allowed).toBe(true);
    expect(result.routedModel).toBe("openai/gpt-4o");
    expect(body.model).toBe("openai/gpt-4o");
  });

  it("leaves an already-pinned request untouched", async () => {
    const key = await makeKey({ forceModel: "openai/gpt-4o" });
    const { enforceApiKeyRestrictions } = await import("@/sse/services/apiKeyEnforcement.js");
    const body = { model: "openai/gpt-4o" };
    const result = await enforceApiKeyRestrictions(key.key, makeRequest(), body);
    expect(result.allowed).toBe(true);
    expect(result.routedModel).toBeNull();
  });

  it("still applies blockedModels to the forced target", async () => {
    const key = await makeKey({ forceModel: "openai/gpt-4o", blockedModels: "openai/gpt-4o" });
    const { enforceApiKeyRestrictions } = await import("@/sse/services/apiKeyEnforcement.js");
    const result = await enforceApiKeyRestrictions(key.key, makeRequest(), { model: "anything" });
    expect(result.allowed).toBe(false);
    expect(result.status).toBe(403);
  });

  it("forceProvider allows its own provider and denies others", async () => {
    const key = await makeKey({ forceProvider: "openai" });
    const { enforceApiKeyRestrictions } = await import("@/sse/services/apiKeyEnforcement.js");
    const ok = await enforceApiKeyRestrictions(key.key, makeRequest(), { model: "openai/gpt-4o" });
    expect(ok.allowed).toBe(true);
    const denied = await enforceApiKeyRestrictions(key.key, makeRequest(), { model: "anthropic/claude-opus" });
    expect(denied.allowed).toBe(false);
    expect(denied.status).toBe(403);
  });

  it("forceProvider fails closed when no provider can be proven", async () => {
    const key = await makeKey({ forceProvider: "openai" });
    const { enforceApiKeyRestrictions } = await import("@/sse/services/apiKeyEnforcement.js");
    // No model at all: nothing proves membership in the pinned provider.
    const empty = await enforceApiKeyRestrictions(key.key, makeRequest(), {});
    expect(empty.allowed).toBe(false);
    expect(empty.status).toBe(403);
    // Inference resolves a claude-* bare id to anthropic, outside the pin.
    const inferred = await enforceApiKeyRestrictions(key.key, makeRequest(), { model: "claude-fake-xyz-123" });
    expect(inferred.allowed).toBe(false);
    expect(inferred.status).toBe(403);
  });

  it("keys without force settings behave exactly as before", async () => {
    const key = await makeKey({});
    const { enforceApiKeyRestrictions } = await import("@/sse/services/apiKeyEnforcement.js");
    const result = await enforceApiKeyRestrictions(key.key, makeRequest(), { model: "openai/gpt-4o" });
    expect(result.allowed).toBe(true);
    expect(result.routedModel).toBeNull();
  });
});
