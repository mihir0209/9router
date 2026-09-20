import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { describe, it, expect, beforeAll, afterAll } from "vitest";

const originalDataDir = process.env.DATA_DIR;
let tempDir;
let db;
let usageRepo;
let auth;
let proxyQuarantine;

let writeCount = 0;

beforeAll(async () => {
  tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "9router-benchmark-"));
  process.env.DATA_DIR = tempDir;
  delete process.env.NINEROUTER_RUNTIME;

  db = await import("@/lib/db/index.js");
  usageRepo = await import("@/lib/db/repos/usageRepo.js");
  auth = await import("@/sse/services/auth.js");
  proxyQuarantine = await import("@/lib/proxy/quarantine.js");
  await db.initDb();

  // Wrap db adapter's run/exec to accurately count all SQL writes
  const adapter = await (await import("@/lib/db/driver.js")).getAdapter();
  const origRun = adapter.run.bind(adapter);
  const origExec = adapter.exec.bind(adapter);

  adapter.run = (sql, ...args) => {
    const s = String(sql).trim().toUpperCase();
    if (s.startsWith("INSERT") || s.startsWith("UPDATE") || s.startsWith("DELETE") || s.startsWith("REPLACE")) {
      writeCount++;
    }
    return origRun(sql, ...args);
  };
  adapter.exec = (sql, ...args) => {
    const stmts = String(sql).split(";");
    for (const st of stmts) {
      const s = st.trim().toUpperCase();
      if (s.startsWith("INSERT") || s.startsWith("UPDATE") || s.startsWith("DELETE") || s.startsWith("REPLACE")) {
        writeCount++;
      }
    }
    return origExec(sql, ...args);
  };
});

afterAll(() => {
  if (tempDir) fs.rmSync(tempDir, { recursive: true, force: true });
  if (originalDataDir === undefined) delete process.env.DATA_DIR;
  else process.env.DATA_DIR = originalDataDir;
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

describe("DB Write Count Benchmark across Modes", () => {
  it("Benchmark 1: Standard API Call (saveRequestUsage)", async () => {
    // 1. Normal mode
    await setLowWrite(false);
    writeCount = 0;
    await db.saveRequestUsage(usageEntry("key-bench-normal", 100));
    const normalWrites = writeCount;

    // 2. Low-write mode (auto Cloudflare mode)
    await setLowWrite(true);
    writeCount = 0;
    await db.saveRequestUsage(usageEntry("key-bench-low", 100));
    const lowWrites = writeCount;

    // 3. Extreme mode
    await setLowWrite("extreme");
    writeCount = 0;
    await db.saveRequestUsage(usageEntry("key-bench-extreme", 100));
    const extremeWrites = writeCount;

    console.log("\n========================================================");
    console.log(" BENCHMARK: Successful Request Usage Logging");
    console.log("========================================================");
    console.log(` Normal Mode:    ${normalWrites} writes (usageHistory + usageDaily + _meta)`);
    console.log(` Low-Write Mode: ${lowWrites} writes (usageDaily + _meta)`);
    console.log(` Extreme Mode:   ${extremeWrites} writes (Pure in-memory, ZERO writes)`);
    console.log("========================================================\n");

    expect(normalWrites).toBe(3);
    expect(lowWrites).toBe(2);
    expect(extremeWrites).toBe(0);
  });

  it("Benchmark 2: Request Detail Observation", async () => {
    const reqRepo = await import("@/lib/db/repos/requestDetailsRepo.js");

    await setLowWrite(false);
    writeCount = 0;
    await reqRepo.saveRequestDetail({
      id: "detail-1",
      provider: "p",
      model: "m",
      timestamp: new Date().toISOString(),
      data: "{}",
    });
    // Let queue flush
    await new Promise((r) => setTimeout(r, 60));
    const normalWrites = writeCount;

    await setLowWrite("extreme");
    writeCount = 0;
    await reqRepo.saveRequestDetail({
      id: "detail-2",
      provider: "p",
      model: "m",
      timestamp: new Date().toISOString(),
      data: "{}",
    });
    await new Promise((r) => setTimeout(r, 60));
    const extremeWrites = writeCount;

    console.log("========================================================");
    console.log(" BENCHMARK: Request Detail / Observability Logging");
    console.log("========================================================");
    console.log(` Normal Mode:    ${normalWrites} writes`);
    console.log(` Extreme Mode:   ${extremeWrites} writes (SKIPPED)`);
    console.log("========================================================\n");

    expect(extremeWrites).toBe(0);
  });

  it("Benchmark 3: Account Failure & Cooldown Locking", async () => {
    const conn = await db.createProviderConnection({
      provider: "openai",
      authType: "apiKey",
      name: "bench-conn",
      data: { apiKey: "test" },
    });

    await setLowWrite(false);
    writeCount = 0;
    await auth.markAccountUnavailable(conn.id, 429, "rate limit", "openai", "gpt-4o");
    const normalWrites = writeCount;

    await setLowWrite("extreme");
    writeCount = 0;
    await auth.markAccountUnavailable(conn.id, 429, "rate limit", "openai", "gpt-4o");
    const extremeWrites = writeCount;

    console.log("========================================================");
    console.log(" BENCHMARK: Account Error & Cooldown Locking");
    console.log("========================================================");
    console.log(` Normal Mode:    ${normalWrites} writes (persisted to providerConnections)`);
    console.log(` Extreme Mode:   ${extremeWrites} writes (In-memory isolate lock only)`);
    console.log("========================================================\n");

    expect(normalWrites).toBe(1);
    expect(extremeWrites).toBe(0);
  });

  it("Benchmark 4: Proxy Pool Quarantine", async () => {
    await setLowWrite(false);
    writeCount = 0;
    await proxyQuarantine.quarantineProxyPool("proxy-123", {
      provider: "openai",
      model: "gpt-4o",
      reason: "rate limited",
      kind: "scoped",
    });
    const normalWrites = writeCount;

    await setLowWrite("extreme");
    writeCount = 0;
    await proxyQuarantine.quarantineProxyPool("proxy-456", {
      provider: "openai",
      model: "gpt-4o",
      reason: "rate limited",
      kind: "scoped",
    });
    const extremeWrites = writeCount;

    console.log("========================================================");
    console.log(" BENCHMARK: Proxy Pool Quarantine");
    console.log("========================================================");
    console.log(` Normal Mode:    ${normalWrites} writes (settings update)`);
    console.log(` Extreme Mode:   ${extremeWrites} writes (In-memory quarantine cache)`);
    console.log("========================================================\n");

    expect(normalWrites).toBe(1);
    expect(extremeWrites).toBe(0);
  });

  it("Benchmark 5: Round-Robin Account Rotation", async () => {
    const c1 = await db.createProviderConnection({
      provider: "openai",
      authType: "apiKey",
      name: "rr-conn-1",
      data: { apiKey: "test1" },
    });
    await db.createProviderConnection({
      provider: "openai",
      authType: "apiKey",
      name: "rr-conn-2",
      data: { apiKey: "test2" },
    });

    // Round-robin strategy (the user's D1 config uses it for 4 providers)
    await db.updateSettings({
      providerStrategies: { openai: { fallbackStrategy: "round-robin", stickyRoundRobinLimit: 1 } },
    });

    await setLowWrite(false);
    writeCount = 0;
    for (let i = 0; i < 4; i++) {
      await auth.getProviderCredentials("openai", new Set(), "gpt-4o");
    }
    const normalWrites = writeCount;

    await setLowWrite("extreme");
    writeCount = 0;
    for (let i = 0; i < 4; i++) {
      const creds = await auth.getProviderCredentials("openai", new Set(), "gpt-4o");
      expect(creds).toBeTruthy();
      expect(creds.connectionId).toBeTruthy();
    }
    const extremeWrites = writeCount;

    await db.updateSettings({ providerStrategies: {} });

    console.log("========================================================");
    console.log(" BENCHMARK: Round-Robin Rotation (4 requests)");
    console.log("========================================================");
    console.log(` Normal Mode:    ${normalWrites} writes (lastUsedAt per request → 4)`);
    console.log(` Extreme Mode:   ${extremeWrites} writes (in-memory rotation registry)`);
    console.log("========================================================\n");

    expect(normalWrites).toBeGreaterThanOrEqual(4);
    expect(extremeWrites).toBe(0);
  });

  it("Benchmark 6: Budget Debit Ledger (force-persisted in extreme)", async () => {
    const budgetService = await import("@/lib/budgetService.js");
    const apiKeysRepo = await import("@/lib/db/repos/apiKeysRepo.js");
    const key = await apiKeysRepo.createApiKey("bench-budget", "bench-machine", {
      budgetUsd: 10,
      budgetResetHours: 24,
    });

    await setLowWrite("extreme");
    writeCount = 0;
    await budgetService.debitBudget(key.key, { requestId: "req-bench-1", commitUsd: 0.05 });
    const extremeWrites = writeCount;

    // Ledger row must be visible from DB (not just memory)
    const state = await budgetService.getBudgetState(key.key);
    expect(state.used).toBeGreaterThan(0);

    console.log("========================================================");
    console.log(" BENCHMARK: Budget Debit Ledger");
    console.log("========================================================");
    console.log(` Extreme Mode:   ${extremeWrites} writes (FORCE-PERSISTED — correctness state, not analytics)`);
    console.log("========================================================\n");

    expect(extremeWrites).toBeGreaterThanOrEqual(1);
    expect(state.used).toBeGreaterThan(0);
  });

  it("Benchmark 7: Model lock registry survives fresh DB reads (extreme)", async () => {
    const c = await db.createProviderConnection({
      provider: "openai",
      authType: "apiKey",
      name: "lock-conn",
      data: { apiKey: "test3" },
    });

    await setLowWrite("extreme");
    writeCount = 0;
    await auth.markAccountUnavailable(c.id, 429, "rate limit", "openai", "gpt-4o");
    expect(writeCount).toBe(0);

    // A fresh credential read must see the lock (the old conn-mutation bug
    // discarded it because getProviderConnections returns new objects).
    const creds = await auth.getProviderCredentials("openai", new Set(), "gpt-4o", {
      excludeConnectionIds: new Set(),
    });
    if (creds) {
      expect(creds.connectionId).not.toBe(c.id);
    }

    // Success clears the registry lock without a DB write
    writeCount = 0;
    await auth.clearAccountError(c.id, { id: c.id, modelLock_gpt_4o: "nope" }, "gpt-4o");
    expect(writeCount).toBe(0);
  });
});
