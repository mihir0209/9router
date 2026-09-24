import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it, expect, beforeAll, afterAll } from "vitest";

const originalDataDir = process.env.DATA_DIR;
const originalRuntime = process.env.NINEROUTER_RUNTIME;
let tempDir;
let db;
let usageRepo;
let auth;
let writeCount = 0;
let readCount = 0;

beforeAll(async () => {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "9router-cache-buffer-"));
  process.env.DATA_DIR = tempDir;
  delete process.env.NINEROUTER_RUNTIME;

  db = await import("@/lib/db/index.js");
  usageRepo = await import("@/lib/db/repos/usageRepo.js");
  auth = await import("@/sse/services/auth.js");
  await db.initDb();

  // Instrument adapter to count read and write SQL statements
  const adapter = await (await import("@/lib/db/driver.js")).getAdapter();
  const origRun = adapter.run.bind(adapter);
  const origAll = adapter.all.bind(adapter);
  const origGet = adapter.get.bind(adapter);

  adapter.run = (sql, ...args) => {
    const s = String(sql).trim().toUpperCase();
    if (s.startsWith("INSERT") || s.startsWith("UPDATE") || s.startsWith("DELETE") || s.startsWith("REPLACE")) {
      writeCount++;
    }
    return origRun(sql, ...args);
  };
  adapter.all = (sql, ...args) => {
    readCount++;
    return origAll(sql, ...args);
  };
  adapter.get = (sql, ...args) => {
    readCount++;
    return origGet(sql, ...args);
  };
});

afterAll(() => {
  if (tempDir) fs.rmSync(tempDir, { recursive: true, force: true });
  if (originalDataDir === undefined) delete process.env.DATA_DIR;
  else process.env.DATA_DIR = originalDataDir;
  if (originalRuntime === undefined) delete process.env.NINEROUTER_RUNTIME;
  else process.env.NINEROUTER_RUNTIME = originalRuntime;
});

function usageEntry(apiKey, promptTokens = 50, completionTokens = 25, timestamp = null) {
  return {
    timestamp: timestamp || new Date().toISOString(),
    provider: "openai",
    model: "gpt-4o",
    connectionId: "conn-1",
    apiKey,
    endpoint: "chat/completions",
    tokens: { prompt_tokens: promptTokens, completion_tokens: completionTokens },
  };
}

describe("5-Minute Credentials & Config Memory Cache", () => {
  it("caches provider connections in memory and avoids DB reads on repeated lookups", async () => {
    db.resetConnectionsCache();
    await db.createProviderConnection({
      provider: "openai",
      authType: "apiKey",
      name: "cached-conn-1",
      apiKey: "sk-test-1",
    });

    readCount = 0;
    // First read: cache miss -> hits DB
    const first = await db.getProviderConnections({ provider: "openai" });
    expect(first.length).toBeGreaterThanOrEqual(1);
    const readsAfterFirst = readCount;
    expect(readsAfterFirst).toBeGreaterThan(0);

    // Repeated reads within 5 minutes: cache hit -> 0 DB reads
    readCount = 0;
    for (let i = 0; i < 5; i++) {
      const cached = await db.getProviderConnections({ provider: "openai" });
      expect(cached.length).toBe(first.length);
    }
    expect(readCount).toBe(0);
  });

  it("invalidates connection cache on mutations", async () => {
    db.resetConnectionsCache();
    const conn = await db.createProviderConnection({
      provider: "anthropic",
      authType: "apiKey",
      name: "cached-anthropic",
      apiKey: "sk-ant-1",
    });

    // Warm cache
    const initial = await db.getProviderConnections({ provider: "anthropic" });
    expect(initial[0].name).toBe("cached-anthropic");

    // Mutation
    await db.updateProviderConnection(conn.id, { name: "updated-anthropic" });

    // Next read should reflect update immediately
    const updated = await db.getProviderConnections({ provider: "anthropic" });
    expect(updated[0].name).toBe("updated-anthropic");
  });

  it("caches API keys in memory for validation and lookups", async () => {
    db.resetApiKeysCache();
    const key = await db.createApiKey("cache-test-key", "test-machine", {
      key: "sk-9r-cache-test-secret-key",
    });

    readCount = 0;
    // First lookup
    const validFirst = await db.validateApiKey(key.key);
    expect(validFirst).toBe(true);
    const readsFirst = readCount;
    expect(readsFirst).toBeGreaterThan(0);

    // Subsequent validations within 5 minutes: hit memory cache, 0 DB reads
    readCount = 0;
    for (let i = 0; i < 5; i++) {
      const valid = await db.validateApiKey(key.key);
      expect(valid).toBe(true);
      const fetched = await db.getApiKeyByKey(key.key);
      expect(fetched.name).toBe("cache-test-key");
    }
    expect(readCount).toBe(0);
  });

  it("caches settings in memory", async () => {
    db.resetSettingsCache();
    await db.updateSettings({ requireLogin: true });

    readCount = 0;
    const s1 = await db.getSettings();
    expect(s1.requireLogin).toBe(true);
    const readsFirst = readCount;
    expect(readsFirst).toBeGreaterThan(0);

    readCount = 0;
    for (let i = 0; i < 5; i++) {
      const s = await db.getSettings();
      expect(s.requireLogin).toBe(true);
    }
    expect(readCount).toBe(0);
  });
});

describe("10-Minute Analytics In-Memory Buffering & Batch Flush", () => {
  beforeAll(async () => {
    process.env.NINEROUTER_RUNTIME = "cloudflare";
    usageRepo.resetLowWriteCache();
    db.resetAnalyticsBuffer();
  });

  afterAll(() => {
    delete process.env.NINEROUTER_RUNTIME;
    usageRepo.resetLowWriteCache();
    db.resetAnalyticsBuffer();
  });

  it("buffers request analytics in memory with ZERO DB writes per request", async () => {
    writeCount = 0;
    const baseTime = Date.now();
    for (let i = 1; i <= 5; i++) {
      const ts = new Date(baseTime + i * 1000).toISOString();
      await db.saveRequestUsage(usageEntry("key-buffered", 100, 50, ts));
    }

    // Every request accumulated in memory: ZERO DB writes!
    expect(writeCount).toBe(0);
    const bufferSize = db.getAnalyticsBufferSize();
    expect(bufferSize.lifetimeRequests).toBe(5);
  });

  it("overlays in-memory buffered stats into getUsageStats and getApiKeyDailyUsage in real-time", async () => {
    // Before flush, read APIs should already reflect the 5 requests (500 prompt, 250 completion tokens)
    const stats = await db.getUsageStats("today");
    expect(stats.totalPromptTokens).toBeGreaterThanOrEqual(500);
    expect(stats.totalCompletionTokens).toBeGreaterThanOrEqual(250);

    const daily = await db.getApiKeyDailyUsage("key-buffered", {});
    expect(daily.length).toBeGreaterThan(0);
    const totalTokens = daily.reduce((sum, r) => sum + r.tokens, 0);
    expect(totalTokens).toBeGreaterThanOrEqual(750);
  });

  it("flushes accumulated analytics to DB in one go with minimal writes", async () => {
    writeCount = 0;
    await db.flushAnalyticsBuffer();

    // After flush: buffer is empty, and only 2 writes happened (usageDaily + _meta)
    const bufferSize = db.getAnalyticsBufferSize();
    expect(bufferSize.lifetimeRequests).toBe(0);
    expect(writeCount).toBe(2);

    // After flush, stats remain correct from DB
    const stats = await db.getUsageStats("today");
    expect(stats.totalPromptTokens).toBeGreaterThanOrEqual(500);
    expect(stats.totalCompletionTokens).toBeGreaterThanOrEqual(250);
  });

  it("forcePersist bypasses buffer and flushes immediately", async () => {
    writeCount = 0;
    await db.saveRequestUsage({
      ...usageEntry("key-force", 20, 10),
      forcePersist: true,
    });
    // forcePersist writes to DB immediately
    expect(writeCount).toBeGreaterThanOrEqual(2);
  });
});
