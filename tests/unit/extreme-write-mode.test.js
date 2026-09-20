import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it, expect, beforeAll, afterAll } from "vitest";

const originalDataDir = process.env.DATA_DIR;
const originalRuntime = process.env.NINEROUTER_RUNTIME;
let tempDir;
let db;
let usageRepo;

beforeAll(async () => {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "9router-extreme-write-"));
  process.env.DATA_DIR = tempDir;
  delete process.env.NINEROUTER_RUNTIME;
  db = await import("@/lib/db/index.js");
  usageRepo = await import("@/lib/db/repos/usageRepo.js");
  await db.initDb();
});

afterAll(() => {
  if (tempDir) fs.rmSync(tempDir, { recursive: true, force: true });
  if (originalDataDir === undefined) delete process.env.DATA_DIR;
  else process.env.DATA_DIR = originalDataDir;
  if (originalRuntime === undefined) delete process.env.NINEROUTER_RUNTIME;
  else process.env.NINEROUTER_RUNTIME = originalRuntime;
});

async function setLowWrite(value) {
  await db.updateSettings({ lowWriteMode: value });
  usageRepo.resetLowWriteCache();
}

function usageEntry(apiKey, n = 10) {
  return {
    timestamp: new Date().toISOString(),
    provider: "testprov",
    model: "testmodel",
    connectionId: "conn-1",
    apiKey,
    endpoint: "chat/completions",
    tokens: { prompt_tokens: n, completion_tokens: n },
  };
}

describe("extreme write mode", () => {
  it("is off by default and does not auto-enable on cloudflare", async () => {
    await setLowWrite(null);
    expect(await usageRepo.isExtremeWriteMode()).toBe(false);
    process.env.NINEROUTER_RUNTIME = "cloudflare";
    usageRepo.resetLowWriteCache();
    // Auto cloudflare => low-write yes, extreme no (explicit opt-in only).
    expect(await usageRepo.isLowWriteMode()).toBe(true);
    expect(await usageRepo.isExtremeWriteMode()).toBe(false);
    delete process.env.NINEROUTER_RUNTIME;
    usageRepo.resetLowWriteCache();
  });

  it("extreme implies low-write mode", async () => {
    await setLowWrite("extreme");
    expect(await usageRepo.isExtremeWriteMode()).toBe(true);
    expect(await usageRepo.isLowWriteMode()).toBe(true);
    await setLowWrite(null);
  });

  it("extreme skips ALL analytics DB writes (history + aggregates + counters)", async () => {
    await setLowWrite("extreme");
    const historyBefore = (await db.getUsageHistory({})).length;
    const statsBefore = await db.getUsageStats("today");

    await db.saveRequestUsage(usageEntry("key-extreme", 42));

    expect((await db.getUsageHistory({})).length).toBe(historyBefore);
    const statsAfter = await db.getUsageStats("today");
    expect(statsAfter.totalPromptTokens).toBe(statsBefore.totalPromptTokens);

    // Still visible in the in-memory ring within the process.
    const logs = await db.getRecentLogs(50);
    expect(logs.some((l) => typeof l === "string" && l.includes("testmodel"))).toBe(true);
    await setLowWrite(null);
  });

  it("plain low-write still keeps daily aggregates", async () => {
    await setLowWrite(true);
    const statsBefore = await db.getUsageStats("today");
    await db.saveRequestUsage(usageEntry("key-low-aggr", 11));
    const statsAfter = await db.getUsageStats("today");
    expect(statsAfter.totalPromptTokens).toBeGreaterThan(statsBefore.totalPromptTokens);
    await setLowWrite(null);
  });
});
