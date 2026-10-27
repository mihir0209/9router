import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

/**
 * The per-isolate read caches (combos, model aliases, custom models, pricing)
 * exist because an uncached kv scan measured ~5ms at production scale and model
 * resolution calls it once per candidate. They are only safe if every write path
 * invalidates them — including the bulk paths that write tables directly.
 */

// Each test builds a fresh adapter (global._dbAdapter is dropped between
// tests), and every adapter registers a process.once("SIGINT") shutdown hook.
// With one adapter per test this file crosses Node's 10-listener warning
// threshold; a production process creates exactly one adapter, so raise the
// limit here rather than "fixing" the driver.
process.setMaxListeners(50);

let tempDir;
const originalDataDir = process.env.DATA_DIR;

beforeEach(() => {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "9router-cacheinv-"));
  process.env.DATA_DIR = tempDir;
  delete global._dbAdapter;
  // The adapter memoises on `global`, which vi.resetModules() does not clear,
  // and the module graph holds a closed handle between tests.
  vi.resetModules();
});

afterEach(() => {
  try { global._dbAdapter?.instance?.close?.(); } catch {}
  delete global._dbAdapter;
  if (tempDir) fs.rmSync(tempDir, { recursive: true, force: true });
  if (originalDataDir === undefined) delete process.env.DATA_DIR;
  else process.env.DATA_DIR = originalDataDir;
});

describe("read caches survive every write path", () => {
  it("importDb is visible to getCombos immediately", async () => {
    const db = await import("@/lib/db/index.js");
    const { getCombos } = await import("@/lib/db/repos/combosRepo.js");

    // Warm the cache with the pre-import state.
    expect(await getCombos()).toEqual([]);
    const now = new Date().toISOString();
    await db.importDb({
      combos: [{ id: "c1", name: "imported", models: ["openai/gpt-4o"], createdAt: now, updatedAt: now }],
    });

    // importDb writes the table directly; without invalidation this stays [].
    const combos = await getCombos();
    expect(combos.map((c) => c.name)).toContain("imported");
  });

  it("importDb is visible to getModelAliases immediately", async () => {
    const db = await import("@/lib/db/index.js");
    const { getModelAliases } = await import("@/lib/db/repos/aliasRepo.js");

    expect(await getModelAliases()).toEqual({});
    await db.importDb({ modelAliases: { sonnet: "anthropic/claude-sonnet-4" } });
    expect(await getModelAliases()).toEqual({ sonnet: "anthropic/claude-sonnet-4" });
  });

  it("importDb is visible to pricing lookups immediately", async () => {
    const db = await import("@/lib/db/index.js");
    const { getPricingForModel } = await import("@/lib/db/repos/pricingRepo.js");

    await db.importDb({ pricing: { openai: { "gpt-4o": { input: 1, output: 2 } } } });
    const pricing = await getPricingForModel("openai", "gpt-4o");
    expect(pricing).toMatchObject({ input: 1, output: 2 });
  });

  it("importDb is visible to getProviderConnections immediately", async () => {
    // Regression: the import used to reset only combos/kv/pricing caches, so a
    // successful restore kept serving the pre-import connections list for up
    // to CREDENTIALS_CACHE_TTL_MS (5 min) — which looked like a rejected import.
    const db = await import("@/lib/db/index.js");
    const { getProviderConnections } = await import("@/lib/db/repos/connectionsRepo.js");

    expect(await getProviderConnections()).toEqual([]); // warm the cache
    const now = new Date().toISOString();
    await db.importDb({
      providerConnections: [{ id: "c1", provider: "openai", authType: "apikey", name: "imported", isActive: true, createdAt: now, updatedAt: now }],
    });
    const conns = await getProviderConnections();
    expect(conns.map((c) => c.name)).toContain("imported");
  });

  it("importDb is visible to getApiKeys immediately", async () => {
    const db = await import("@/lib/db/index.js");
    const { getApiKeys } = await import("@/lib/db/repos/apiKeysRepo.js");

    expect(await getApiKeys()).toEqual([]); // warm the cache
    const now = new Date().toISOString();
    await db.importDb({
      apiKeys: [{ id: "k1", key: "sk-imported", name: "imported", isActive: true, createdAt: now, updatedAt: now }],
    });
    const keys = await getApiKeys();
    expect(keys.map((k) => k.name)).toContain("imported");
  });

  it("importDb is visible to the enforcement path (getApiKeyByKey) immediately", async () => {
    const db = await import("@/lib/db/index.js");
    const { getApiKeyByKey } = await import("@/lib/db/repos/apiKeysRepo.js");

    expect(await getApiKeyByKey("sk-imported")).toBeNull(); // warm the cache
    const now = new Date().toISOString();
    await db.importDb({
      apiKeys: [{ id: "k1", key: "sk-imported", name: "imported", isActive: true, createdAt: now, updatedAt: now }],
    });
    expect((await getApiKeyByKey("sk-imported"))?.id).toBe("k1");
  });

  it("importDb is visible to getProviderNodes immediately", async () => {
    const db = await import("@/lib/db/index.js");
    const { getProviderNodes } = await import("@/lib/db/repos/nodesRepo.js");

    expect(await getProviderNodes()).toEqual([]); // warm the cache
    const now = new Date().toISOString();
    await db.importDb({
      providerNodes: [{ id: "n1", type: "openai-compatible", name: "imported-node", createdAt: now, updatedAt: now, data: {} }],
    });
    const nodes = await getProviderNodes();
    expect(nodes.map((n) => n.name)).toContain("imported-node");
  });

  it("importDb is visible to getSettings immediately", async () => {
    const db = await import("@/lib/db/index.js");
    const { getSettings } = await import("@/lib/db/repos/settingsRepo.js");

    const before = await getSettings(); // warm the cache with defaults
    expect(before.comboStrategy).not.toBe("fusion");
    await db.importDb({ settings: { comboStrategy: "fusion" } });
    expect((await getSettings()).comboStrategy).toBe("fusion");
  });

  it("resetPricingCache drops the cache without deleting stored rates", async () => {
    // Guards the footgun this work introduced: pricingRepo also exports
    // resetPricing(), which DELETES a provider's overrides. The cache clear
    // used after an import must not be that one, or the import erases itself.
    const { getAdapter } = await import("../../src/lib/db/driver.js");
    const { updatePricing, resetPricingCache } = await import("@/lib/db/repos/pricingRepo.js");
    const db = await getAdapter();

    await updatePricing({ openai: { "gpt-4o": { input: 7, output: 9 } } });
    resetPricingCache();
    const row = db.get("SELECT value FROM kv WHERE scope = 'pricing' AND key = ?", ["openai"]);
    expect(row, "resetPricingCache must not delete stored rates").toBeTruthy();
  });

  it("a direct setModelAlias invalidates the cache", async () => {
    const { getModelAliases, setModelAlias } = await import("@/lib/db/repos/aliasRepo.js");
    expect(await getModelAliases()).toEqual({});
    await setModelAlias("sonnet", "anthropic/claude-sonnet-4");
    expect((await getModelAliases()).sonnet).toBe("anthropic/claude-sonnet-4");
  });

  it("a direct createCombo invalidates the cache", async () => {
    const { getCombos, createCombo } = await import("@/lib/db/repos/combosRepo.js");
    expect(await getCombos()).toEqual([]);
    await createCombo({ name: "fresh", models: ["openai/gpt-4o"] });
    expect((await getCombos()).map((c) => c.name)).toContain("fresh");
  });
});
