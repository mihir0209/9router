/**
 * A pin written the way the /api/keys routes write it (validateForcePins ->
 * createApiKey / updateApiKey) is persisted in a real DB and then enforced by
 * enforceApiKeyRestrictions.
 */
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";

const originalDataDir = process.env.DATA_DIR;
let tempDir;
let db;
let validateForcePins;
let enforceApiKeyRestrictions;

const MACHINE = "0123456789abcdef";
const request = { url: "http://localhost/v1/chat/completions", headers: { get: () => null } };

beforeAll(async () => {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "9router-pins-"));
  process.env.DATA_DIR = tempDir;
  delete global._dbAdapter;
  vi.resetModules();
  db = await import("@/lib/db/index.js");
  await db.initDb();
  ({ validateForcePins } = await import("@/lib/apiKeys/forcePins.js"));
  ({ enforceApiKeyRestrictions } = await import("@/sse/services/apiKeyEnforcement.js"));
});

afterAll(() => {
  try { global._dbAdapter?.instance?.close?.(); } catch {}
  delete global._dbAdapter;
  if (tempDir) fs.rmSync(tempDir, { recursive: true, force: true });
  if (originalDataDir === undefined) delete process.env.DATA_DIR;
  else process.env.DATA_DIR = originalDataDir;
});

describe("key pins end to end", () => {
  it("persists a pin written on create and enforces it", async () => {
    const { values, error } = validateForcePins({ forceModel: "  openai/gpt-4o " });
    expect(error).toBeNull();
    const created = await db.createApiKey("pin-create", MACHINE, { key: "sk-pin-create", ...values });

    const stored = await db.getApiKeyById(created.id);
    expect(stored.forceModel).toBe("openai/gpt-4o");

    const body = { model: "anthropic/claude-opus" };
    const result = await enforceApiKeyRestrictions("sk-pin-create", request, body);
    expect(result.allowed).toBe(true);
    expect(result.routedModel).toBe("openai/gpt-4o");
    expect(body.model).toBe("openai/gpt-4o");
  });

  it("persists a pin set by update, then enforces it, then stops once cleared", async () => {
    const created = await db.createApiKey("pin-update", MACHINE, { key: "sk-pin-update" });

    const set = validateForcePins({ forceModel: "openai/gpt-4o" });
    await db.updateApiKey(created.id, set.values);
    expect((await db.getApiKeyById(created.id)).forceModel).toBe("openai/gpt-4o");

    let body = { model: "other/model" };
    let result = await enforceApiKeyRestrictions("sk-pin-update", request, body);
    expect(result.routedModel).toBe("openai/gpt-4o");

    const cleared = validateForcePins({ forceModel: "" });
    await db.updateApiKey(created.id, cleared.values);
    expect((await db.getApiKeyById(created.id)).forceModel).toBe("");

    body = { model: "other/model" };
    result = await enforceApiKeyRestrictions("sk-pin-update", request, body);
    expect(result.routedModel).toBeNull();
    expect(body.model).toBe("other/model");
  });

  it("enforces a persisted forceProvider", async () => {
    const { values } = validateForcePins({ forceProvider: "openai" });
    await db.createApiKey("pin-provider", MACHINE, { key: "sk-pin-provider", ...values });
    expect((await db.getApiKeyByKey("sk-pin-provider")).forceProvider).toBe("openai");

    const denied = await enforceApiKeyRestrictions("sk-pin-provider", request, { model: "anthropic/claude-opus" });
    expect(denied.allowed).toBe(false);
    expect(denied.error).toBe("This API key is pinned to provider 'openai'");
  });
});

describe("pair check agrees with enforcement", () => {
  it("rejects a mismatching pair at write time exactly where enforcement would 403", async () => {
    const { findForcePairMismatch } = await import("@/lib/apiKeys/forcePair.js");
    expect(await findForcePairMismatch({ forceProvider: "anthropic", forceModel: "openai/gpt-4o" })).toMatch(/does not match forceProvider 'anthropic'/);
    expect(await findForcePairMismatch({ forceProvider: "openai", forceModel: "openai/gpt-4o" })).toBeNull();

    const bad = await db.createApiKey("pair-bad", MACHINE, { key: "sk-pair-bad", forceProvider: "anthropic", forceModel: "openai/gpt-4o" });
    const denied = await enforceApiKeyRestrictions(bad.key, request, { model: "openai/gpt-4o" });
    expect(denied.allowed).toBe(false);
    expect(denied.status).toBe(403);

    const good = await db.createApiKey("pair-good", MACHINE, { key: "sk-pair-good", forceProvider: "openai", forceModel: "openai/gpt-4o" });
    expect((await enforceApiKeyRestrictions(good.key, request, { model: "x" })).allowed).toBe(true);
  });

  it("a bare forceModel conflicts with a provider-prefixed blocked entry, and enforcement agrees", async () => {
    const { values, error } = validateForcePins({ forceModel: "openai/gpt-4o" }, { forceModel: "openai/gpt-4o", blockedModels: "openai/gpt-4o" });
    expect(error).toBe("forceModel must not be in blockedModels");
    expect(values).toEqual({});
    const key = await db.createApiKey("pair-blocked", MACHINE, { key: "sk-pair-blocked", forceModel: "openai/gpt-4o", blockedModels: "openai/gpt-4o" });
    const r = await enforceApiKeyRestrictions(key.key, request, { model: "x" });
    expect(r.allowed).toBe(false);
  });
});
