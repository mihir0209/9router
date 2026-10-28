import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

/**
 * importDb used to finish with a full exportDb() whose result no caller read.
 * It now returns row counts and only builds a snapshot on request. The caches
 * must still be reset either way (see db-cache-invalidation.test.js).
 */

process.setMaxListeners(50);

let tempDir;
const originalDataDir = process.env.DATA_DIR;
const originalRuntime = process.env.NINEROUTER_RUNTIME;

beforeEach(() => {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "9router-importret-"));
  process.env.DATA_DIR = tempDir;
  delete process.env.NINEROUTER_RUNTIME;
  delete global._dbAdapter;
  vi.resetModules();
});

afterEach(() => {
  try { global._dbAdapter?.instance?.close?.(); } catch {}
  delete global._dbAdapter;
  if (tempDir) fs.rmSync(tempDir, { recursive: true, force: true });
  if (originalDataDir === undefined) delete process.env.DATA_DIR;
  else process.env.DATA_DIR = originalDataDir;
  if (originalRuntime === undefined) delete process.env.NINEROUTER_RUNTIME;
  else process.env.NINEROUTER_RUNTIME = originalRuntime;
});

function samplePayload() {
  const now = new Date().toISOString();
  return {
    settings: { cloudEnabled: false },
    providerConnections: [
      { id: "pc1", provider: "openai", authType: "apikey", name: "one", apiKey: "sk-1", createdAt: now, updatedAt: now },
      { id: "pc2", provider: "openai", authType: "apikey", name: "two", apiKey: "sk-2", createdAt: now, updatedAt: now },
    ],
    apiKeys: [{ id: "k1", key: "sk-test-1", name: "first", createdAt: now }],
    combos: [{ id: "c1", name: "combo-one", models: ["openai/gpt-4o"], createdAt: now, updatedAt: now }],
    modelAliases: { sonnet: "anthropic/claude-sonnet-4", fast: "openai/gpt-4o-mini" },
  };
}

describe("importDb return value", () => {
  it("returns success and per-table counts without a snapshot by default", async () => {
    const db = await import("@/lib/db/index.js");
    const result = await db.importDb(samplePayload());

    expect(result.success).toBe(true);
    expect(result).not.toHaveProperty("snapshot");
    expect(result.counts).toEqual({
      settings: 1,
      providerConnections: 2,
      providerNodes: 0,
      proxyPools: 0,
      apiKeys: 1,
      combos: 1,
      modelAliases: 2,
      customModels: 0,
      mitmAlias: 0,
      pricing: 0,
    });
  });

  it("includes a snapshot equal to exportDb() when returnSnapshot is true", async () => {
    const db = await import("@/lib/db/index.js");
    const result = await db.importDb(samplePayload(), { returnSnapshot: true });

    expect(result.success).toBe(true);
    expect(result.snapshot).toEqual(await db.exportDb());
    expect(result.snapshot.combos.map((c) => c.name)).toEqual(["combo-one"]);
  });

  it("does not return a snapshot on Cloudflare even when asked", async () => {
    process.env.NINEROUTER_RUNTIME = "cloudflare";
    const db = await import("@/lib/db/index.js");
    const result = await db.importDb(samplePayload(), { returnSnapshot: true });

    expect(result.success).toBe(true);
    expect(result).not.toHaveProperty("snapshot");
    expect(result.counts.combos).toBe(1);
  });

  it("only treats returnSnapshot === true as opt-in", async () => {
    const db = await import("@/lib/db/index.js");
    const result = await db.importDb(samplePayload(), { returnSnapshot: "yes" });
    expect(result).not.toHaveProperty("snapshot");
  });

  it("still rejects an invalid payload", async () => {
    const db = await import("@/lib/db/index.js");
    await expect(db.importDb(null)).rejects.toThrow("Invalid database payload");
    await expect(db.importDb([])).rejects.toThrow("Invalid database payload");
  });
});

describe("importDb still resets read caches without the re-export", () => {
  it("makes imported api keys, combos and aliases visible immediately", async () => {
    const db = await import("@/lib/db/index.js");
    const { getApiKeys } = await import("@/lib/db/repos/apiKeysRepo.js");
    const { getCombos } = await import("@/lib/db/repos/combosRepo.js");
    const { getModelAliases } = await import("@/lib/db/repos/aliasRepo.js");

    // Prime every cache with the empty database first.
    expect(await getApiKeys()).toEqual([]);
    expect(await getCombos()).toEqual([]);
    expect(await getModelAliases()).toEqual({});

    await db.importDb(samplePayload());

    expect((await getApiKeys()).map((k) => k.id)).toEqual(["k1"]);
    expect((await getCombos()).map((c) => c.name)).toEqual(["combo-one"]);
    expect(await getModelAliases()).toEqual({
      sonnet: "anthropic/claude-sonnet-4",
      fast: "openai/gpt-4o-mini",
    });
  });
});
