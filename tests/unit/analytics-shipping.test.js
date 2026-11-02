import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it, expect, beforeAll, afterAll } from "vitest";

const originalDataDir = process.env.DATA_DIR;
const originalRuntime = process.env.NINEROUTER_RUNTIME;
let tempDir;
let db;
let usageRepo;
let requestDetailsRepo;

beforeAll(async () => {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "9router-analytics-shipping-"));
  process.env.DATA_DIR = tempDir;
  delete process.env.NINEROUTER_RUNTIME;

  db = await import("@/lib/db/index.js");
  usageRepo = await import("@/lib/db/repos/usageRepo.js");
  requestDetailsRepo = await import("@/lib/db/repos/requestDetailsRepo.js");
  await db.initDb();
});

afterAll(() => {
  if (tempDir) fs.rmSync(tempDir, { recursive: true, force: true });
  if (originalDataDir === undefined) delete process.env.DATA_DIR;
  else process.env.DATA_DIR = originalDataDir;
  if (originalRuntime === undefined) delete process.env.NINEROUTER_RUNTIME;
  else process.env.NINEROUTER_RUNTIME = originalRuntime;
});

function makeUsage(apiKey = "key-ship-test", prompt = 120, completion = 45, cost = 0.005) {
  return {
    timestamp: new Date().toISOString(),
    provider: "openai",
    model: "gpt-4o",
    connectionId: "conn-shipping",
    apiKey,
    endpoint: "chat/completions",
    cost,
    tokens: { prompt_tokens: prompt, completion_tokens: completion },
  };
}

describe("End-to-End Analytics Shipping Verification", () => {
  it("ships analytics directly to SQLite DB in normal mode (lowWriteMode=false)", async () => {
    await db.updateSettings({ lowWriteMode: false, analyticsBufferEnabled: false });
    usageRepo.resetLowWriteCache();
    db.resetAnalyticsBuffer();

    const adapter = await (await import("@/lib/db/driver.js")).getAdapter();
    const initialMeta = adapter.get("SELECT value FROM _meta WHERE key = 'totalRequestsLifetime'");
    const initialLifetime = initialMeta ? parseInt(initialMeta.value, 10) : 0;
    const initialHistoryCount = (await db.getUsageHistory({})).length;

    const entry = makeUsage("key-normal-1", 150, 75, 0.012);
    await db.saveRequestUsage(entry);

    // 1. Verify usageHistory table
    const history = await db.getUsageHistory({});
    expect(history.length).toBe(initialHistoryCount + 1);
    const lastHistory = history[history.length - 1];
    expect(lastHistory.provider).toBe("openai");
    expect(lastHistory.model).toBe("gpt-4o");
    expect(lastHistory.cost).toBeCloseTo(0.012);
    expect(lastHistory.tokens.prompt_tokens).toBe(150);
    expect(lastHistory.tokens.completion_tokens).toBe(75);

    // 2. Verify _meta table totalRequestsLifetime
    const updatedMeta = adapter.get("SELECT value FROM _meta WHERE key = 'totalRequestsLifetime'");
    expect(parseInt(updatedMeta.value, 10)).toBe(initialLifetime + 1);

    // 3. Verify usageDaily aggregate table
    const dateKey = entry.timestamp.slice(0, 10);
    const dailyRow = adapter.get("SELECT data FROM usageDaily WHERE dateKey = ?", [dateKey]);
    expect(dailyRow).toBeDefined();
    const dayData = JSON.parse(dailyRow.data);
    expect(dayData.requests).toBeGreaterThanOrEqual(1);
    expect(dayData.promptTokens).toBeGreaterThanOrEqual(150);
    expect(dayData.completionTokens).toBeGreaterThanOrEqual(75);
    expect(dayData.cost).toBeGreaterThanOrEqual(0.012);

    // 4. Verify getUsageStats('today') reflects exactly
    const stats = await db.getUsageStats("today");
    expect(stats.totalPromptTokens).toBeGreaterThanOrEqual(150);
    expect(stats.totalCompletionTokens).toBeGreaterThanOrEqual(75);
    expect(stats.byProvider["openai"]).toBeDefined();
  });

  it("ships aggregates and lifetime counter to DB in low-write mode while skipping per-request history", async () => {
    await db.updateSettings({ lowWriteMode: true, analyticsBufferEnabled: false });
    usageRepo.resetLowWriteCache();

    const adapter = await (await import("@/lib/db/driver.js")).getAdapter();
    const initialMeta = adapter.get("SELECT value FROM _meta WHERE key = 'totalRequestsLifetime'");
    const initialLifetime = initialMeta ? parseInt(initialMeta.value, 10) : 0;
    const initialHistoryCount = (await db.getUsageHistory({})).length;

    const entry = makeUsage("key-low-1", 200, 100, 0.02);
    await db.saveRequestUsage(entry);

    // 1. usageHistory count must not increase
    const historyAfter = (await db.getUsageHistory({})).length;
    expect(historyAfter).toBe(initialHistoryCount);

    // 2. _meta lifetime counter must still increment
    const updatedMeta = adapter.get("SELECT value FROM _meta WHERE key = 'totalRequestsLifetime'");
    expect(parseInt(updatedMeta.value, 10)).toBe(initialLifetime + 1);

    // 3. getUsageStats must still show aggregated tokens
    const stats = await db.getUsageStats("today");
    expect(stats.totalPromptTokens).toBeGreaterThanOrEqual(350); // 150 + 200
    expect(stats.totalCompletionTokens).toBeGreaterThanOrEqual(175); // 75 + 100
  });

  it("records requestDetails and flushes buffer properly when observability is enabled", async () => {
    await db.updateSettings({ enableObservability: true, lowWriteMode: false });
    usageRepo.resetLowWriteCache();

    const reqId = "req-obs-" + Date.now();
    await db.saveRequestDetail({
      id: reqId,
      timestamp: new Date().toISOString(),
      model: "gpt-4o",
      provider: "openai",
      status: 200,
      durationMs: 250,
      tokens: { prompt_tokens: 80, completion_tokens: 40 },
    });

    // Reading request detail should flush any pending buffer and return record
    const detail = await db.getRequestDetailById(reqId);
    expect(detail).toBeDefined();
    expect(detail.id).toBe(reqId);
    expect(detail.model).toBe("gpt-4o");
    expect(detail.provider).toBe("openai");

    const res = await db.getRequestDetails({ limit: 10 });
    expect(res.details.some((r) => r.id === reqId)).toBe(true);
  });

  it("exportDb flushes all analytics buffers and exports database payload", async () => {
    // Stage buffered data
    db.resetAnalyticsBuffer();
    await db.updateSettings({ analyticsBufferEnabled: true, lowWriteMode: false });
    usageRepo.resetLowWriteCache();

    const bufferedEntry = makeUsage("key-buf-export", 300, 150, 0.05);
    await db.saveRequestUsage(bufferedEntry);

    // Ensure it is in memory buffer
    const bufSize = db.getAnalyticsBufferSize();
    expect(bufSize.lifetimeRequests).toBe(1);

    // exportDb must flush buffer first
    const exported = await db.exportDb();
    expect(exported).toBeDefined();
    expect(exported.settings).toBeDefined();
    expect(Array.isArray(exported.apiKeys)).toBe(true);

    // Verify buffer was flushed to 0
    const bufSizeAfter = db.getAnalyticsBufferSize();
    expect(bufSizeAfter.lifetimeRequests).toBe(0);

    // Verify the flushed data landed in DB
    const adapter = await (await import("@/lib/db/driver.js")).getAdapter();
    const history = await db.getUsageHistory({});
    expect(history.some((h) => h.apiKeyMasked && h.apiKeyMasked.startsWith("key-buf-"))).toBe(true);

    const rawHistoryRow = adapter.get("SELECT apiKey FROM usageHistory WHERE apiKey = 'key-buf-export'");
    expect(rawHistoryRow).toBeDefined();
    expect(rawHistoryRow.apiKey).toBe("key-buf-export");

    const meta = adapter.get("SELECT value FROM _meta WHERE key = 'totalRequestsLifetime'");
    expect(meta).toBeDefined();
    expect(parseInt(meta.value, 10)).toBeGreaterThanOrEqual(1);
  });
});
