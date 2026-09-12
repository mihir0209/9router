import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it, expect, beforeAll, afterAll } from "vitest";

const originalDataDir = process.env.DATA_DIR;
let tempDir;
let db;

beforeAll(async () => {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "9router-custom-keys-"));
  process.env.DATA_DIR = tempDir;
  db = await import("@/lib/db/index.js");
  await db.initDb();
});

afterAll(() => {
  if (tempDir) fs.rmSync(tempDir, { recursive: true, force: true });
  if (originalDataDir === undefined) delete process.env.DATA_DIR;
  else process.env.DATA_DIR = originalDataDir;
});

describe("custom API key values", () => {
  it("createApiKey uses a provided custom key value", async () => {
    const k = await db.createApiKey("license-key", "machine-abc", { key: "LIC-12345-ABCDE" });
    expect(k.key).toBe("LIC-12345-ABCDE");
    expect(await db.validateApiKey("LIC-12345-ABCDE")).toBeTruthy();
    expect((await db.getApiKeyByKey("LIC-12345-ABCDE"))?.id).toBe(k.id);
  });

  it("createApiKey trims the custom value and rejects duplicates", async () => {
    await expect(db.createApiKey("dup", "machine-abc", { key: "  LIC-12345-ABCDE  " }))
      .rejects.toMatchObject({ code: "DUPLICATE_API_KEY" });
  });

  it("createApiKey still auto-generates when no custom value is given", async () => {
    const k = await db.createApiKey("auto-key", "machine-abc");
    expect(k.key).toMatch(/^sk-/);
  });

  it("updateApiKey replaces the value with a custom key and remaps history", async () => {
    const k = await db.createApiKey("rotating", "machine-abc");
    await db.saveRequestUsage({
      timestamp: new Date().toISOString(),
      provider: "p", model: "m", apiKey: k.key,
      tokens: { prompt_tokens: 5, completion_tokens: 5 },
    });
    const updated = await db.updateApiKey(k.id, { key: "LIC-ROTATED-1" });
    expect(updated.key).toBe("LIC-ROTATED-1");
    expect(await db.validateApiKey("LIC-ROTATED-1")).toBeTruthy();
    expect(await db.validateApiKey(k.key)).toBeFalsy();
    const hist = await db.getUsageHistory({});
    expect(hist.some((h) => h.apiKeyMasked === "LIC-ROTA***")).toBe(true);
  });

  it("updateApiKey rejects a duplicate custom value and keeps the current one on empty", async () => {
    const a = await db.createApiKey("ka", "machine-abc", { key: "LIC-A" });
    const b = await db.createApiKey("kb", "machine-abc", { key: "LIC-B" });
    await expect(db.updateApiKey(b.id, { key: "LIC-A" }))
      .rejects.toMatchObject({ code: "DUPLICATE_API_KEY" });
    const kept = await db.updateApiKey(a.id, { key: "   " });
    expect(kept.key).toBe("LIC-A");
    const same = await db.updateApiKey(a.id, { key: "LIC-A" });
    expect(same.key).toBe("LIC-A");
  });
});
