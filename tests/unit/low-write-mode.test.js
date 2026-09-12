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
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "9router-low-write-"));
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

describe("low-write analytics mode", () => {
  it("is off locally by default (auto), on for the cloudflare runtime", async () => {
    await setLowWrite(null);
    expect(await usageRepo.isLowWriteMode()).toBe(false);
    process.env.NINEROUTER_RUNTIME = "cloudflare";
    usageRepo.resetLowWriteCache();
    expect(await usageRepo.isLowWriteMode()).toBe(true);
    delete process.env.NINEROUTER_RUNTIME;
    usageRepo.resetLowWriteCache();
    expect(await usageRepo.isLowWriteMode()).toBe(false);
  });

  it("explicit false wins on cloudflare, explicit true wins locally", async () => {
    process.env.NINEROUTER_RUNTIME = "cloudflare";
    await setLowWrite(false);
    expect(await usageRepo.isLowWriteMode()).toBe(false);
    delete process.env.NINEROUTER_RUNTIME;
    await setLowWrite(true);
    expect(await usageRepo.isLowWriteMode()).toBe(true);
    await setLowWrite(null);
  });

  it("normal mode records per-request history rows", async () => {
    await setLowWrite(false);
    const before = (await db.getUsageHistory({})).length;
    await db.saveRequestUsage(usageEntry("key-normal"));
    const after = (await db.getUsageHistory({})).length;
    expect(after).toBe(before + 1);
  });

  it("low-write mode skips history rows but keeps daily aggregates + stats", async () => {
    await setLowWrite(true);
    const before = (await db.getUsageHistory({})).length;
    const t0 = new Date(Date.now() - 2000).toISOString();
    const t1 = new Date().toISOString();
    await db.saveRequestUsage({ ...usageEntry("key-low", 25), timestamp: t0 });
    await db.saveRequestUsage({ ...usageEntry("key-low", 30), timestamp: t1 });
    expect((await db.getUsageHistory({})).length).toBe(before);

    const stats = await db.getUsageStats("today");
    expect(stats.totalPromptTokens).toBeGreaterThanOrEqual(55);
    expect(stats.byApiKey && Object.keys(stats.byApiKey).length).toBeGreaterThan(0);

    const daily = await db.getApiKeyDailyUsage("key-low", {});
    expect(daily.length).toBeGreaterThan(0);
    const total = daily.reduce((s, r) => s + r.tokens, 0);
    expect(total).toBeGreaterThanOrEqual(110);
  });

  it("low-write mode serves recent logs from the in-memory ring", async () => {
    await setLowWrite(true);
    await db.saveRequestUsage(usageEntry("key-ring", 7));
    const logs = await db.getRecentLogs(50);
    expect(Array.isArray(logs)).toBe(true);
    expect(logs.some((l) => typeof l === "string" && l.includes("testmodel"))).toBe(true);
  });

  it("request details are skipped in low-write mode", async () => {
    await db.updateSettings({ enableObservability: true });
    await setLowWrite(true);
    await db.saveRequestDetail({ id: "d1", provider: "p", model: "m", timestamp: new Date().toISOString() });
    await new Promise((r) => setTimeout(r, 100));
    const res = await db.getRequestDetails({});
    expect(res.details.length).toBe(0);
    await db.updateSettings({ enableObservability: false });
    await setLowWrite(null);
  });
});
