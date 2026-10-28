// End-to-end harness that runs the REAL chat pipeline (handleChat -> auth ->
// chatCore -> executor -> usage persistence) in Cloudflare mode against a
// counting fake D1 and a local mock upstream, so the D1 cost of one proxied
// request is measured rather than estimated.
//
// Usage (inside a vitest file that has already vi.mock'ed
// "@opennextjs/cloudflare" with `holder`):
//   const h = createHarness(holder);
//   await h.seed();                 // writes a production-sized dataset to D1
//   await h.coldStart();            // drops every in-process cache + the adapter
//   const r = await h.measure("name", () => h.chat({ stream: false }));
import { createCountingD1 } from "./countingD1.js";
import { startMockUpstream } from "../../scripts/bench/mock-upstream.mjs";

export const BENCH_KEY = "sk-bench-0123456789abcdef";
export const PINNED_KEY = "sk-bench-pinned-0123456789";
export const BUDGET_KEY = "sk-bench-budget-0123456789";
export const LIMITS_KEY = "sk-bench-limits-0123456789";
export const RATE_KEY = "sk-bench-rate-0123456789ab";
export const CONC_KEY = "sk-bench-conc-0123456789ab";

export function createHarness(holder, { dataset = {} } = {}) {
  const d1 = createCountingD1();
  holder.d1 = d1;
  holder.pending = [];
  const baseGlobals = new Set(Object.getOwnPropertyNames(globalThis));
  let mock = null;
  let handleChatFn = null;
  let modules = null;

  const sizes = { connections: 42, apiKeys: 5, combos: 8, pools: 31, customModels: 5291, usageDays: 30, ...dataset };

  async function settle() {
    // Persist tasks are handed to ctx.waitUntil; wait for all of them (and any
    // they spawn) the way the Workers runtime keeps the isolate alive for them.
    for (let guard = 0; guard < 50 && holder.pending.length; guard++) {
      const batch = holder.pending.splice(0);
      await Promise.allSettled(batch);
    }
    await new Promise((r) => setTimeout(r, 10));
    for (let guard = 0; guard < 50 && holder.pending.length; guard++) {
      const batch = holder.pending.splice(0);
      await Promise.allSettled(batch);
    }
  }

  /** Drop everything a fresh Workers isolate would not have; keep D1 content. */
  async function coldStart() {
    await settle();
    for (const name of Object.getOwnPropertyNames(globalThis)) {
      if (!baseGlobals.has(name) && name.startsWith("_")) delete globalThis[name];
    }
    delete global._dbAdapter;
    const { vi } = await import("vitest");
    vi.resetModules();
    modules = null;
    handleChatFn = null;
  }

  async function load() {
    if (modules) return modules;
    const db = await import("@/lib/db/index.js");
    const usageRepo = await import("@/lib/db/repos/usageRepo.js");
    modules = { db, usageRepo };
    return modules;
  }

  async function chatHandler() {
    if (handleChatFn) return handleChatFn;
    const { initTranslators } = await import("open-sse/translator/index.js");
    await initTranslators();
    ({ handleChat: handleChatFn } = await import("@/sse/handlers/chat.js"));
    return handleChatFn;
  }

  async function ensureMock() {
    if (!mock) mock = await startMockUpstream({ sseChunks: 6 });
    return mock;
  }

  async function seed() {
    const m = await ensureMock();
    const { db } = await load();
    await db.initDb();
    const now = new Date().toISOString();
    const connections = [];
    // The connection the bench actually routes to.
    connections.push({
      id: "conn-mock", provider: "openai-compatible-mock", authType: "apikey", name: "mock", priority: 1, isActive: true,
      apiKey: "sk-upstream-mock", providerSpecificData: { baseUrl: `${m.url}/v1`, prefix: "mock", apiType: "chat" },
      createdAt: now, updatedAt: now,
    });
    for (let i = 1; i < sizes.connections; i++) {
      connections.push({
        id: `conn-${i}`, provider: i % 3 ? "openai" : "anthropic", authType: "apikey", name: `acct-${i}`, priority: i + 1, isActive: true,
        apiKey: `sk-filler-${i}`, createdAt: now, updatedAt: now,
      });
    }
    const apiKeys = [];
    for (let i = 0; i < sizes.apiKeys; i++) {
      apiKeys.push({ id: `key-${i}`, key: i === 0 ? BENCH_KEY : `sk-other-${i}`, name: `key ${i}`, machineId: "bench", isActive: true, createdAt: now });
    }
    const policyKey = (id, key, extra) => apiKeys.push({ id, key, name: id, machineId: "bench", isActive: true, createdAt: now, ...extra });
    policyKey("key-budget", BUDGET_KEY, { budgetUsd: 1000, budgetResetHours: 720 });
    policyKey("key-limits", LIMITS_KEY, { tokenLimitDaily: 1_000_000_000, tokenLimitMonthly: 1_000_000_000 });
    policyKey("key-rate", RATE_KEY, { rateLimitRequests: 1_000_000, rateLimitWindowSeconds: 60 });
    policyKey("key-conc", CONC_KEY, { maxConcurrentRequests: 1000 });
    apiKeys.push({
      id: "key-pinned", key: PINNED_KEY, name: "pinned", machineId: "bench", isActive: true, createdAt: now,
      forceModel: "mock/mock-model", forceProvider: "openai-compatible-mock", blockedModels: "mock/forbidden",
      allowedEndpoints: "chat.completions",
    });
    const combos = Array.from({ length: sizes.combos }, (_, i) => ({ id: `combo-${i}`, name: `combo-${i}`, models: ["mock/mock-model"], createdAt: now, updatedAt: now }));
    const proxyPools = Array.from({ length: sizes.pools }, (_, i) => ({ id: `pool-${i}`, name: `pool-${i}`, proxyUrl: `http://127.0.0.1:${9000 + i}`, isActive: true, testStatus: "unknown", createdAt: now, updatedAt: now }));
    const customModels = Array.from({ length: sizes.customModels }, (_, i) => ({ providerAlias: `p${i % 40}`, id: `model-${i}`, type: "llm", name: `Model ${i}` }));
    await db.importDb({
      settings: { requireApiKey: true, cloudEnabled: false, ...(sizes.settings || {}) },
      providerConnections: connections,
      providerNodes: [{ id: "openai-compatible-mock", type: "openai-compatible", name: "mock", prefix: "mock", apiType: "chat", baseUrl: `${m.url}/v1`, createdAt: now, updatedAt: now }],
      proxyPools,
      apiKeys,
      combos,
      customModels,
      modelAliases: {},
    });
    await settle();
    // Realistic daily history so the cold start has something to read.
    const adapter = await (await import("@/lib/db/driver.js")).getAdapter();
    adapter.transaction(() => {
      for (let i = 0; i < sizes.usageDays; i++) {
        const d = new Date(Date.now() - i * 86400000);
        const dateKey = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
        adapter.run(`INSERT INTO usageDaily(dateKey, data) VALUES(?, ?) ON CONFLICT(dateKey) DO UPDATE SET data = excluded.data`, [dateKey, JSON.stringify({ requests: 100, promptTokens: 1000, completionTokens: 1000, cost: 1, byProvider: {}, byModel: {}, byAccount: {}, byApiKey: {}, byEndpoint: {} })]);
      }
    });
    await settle();
  }

  function request({ stream = false, key = BENCH_KEY, model = "mock/mock-model", failUpstream = null } = {}) {
    const headers = { "content-type": "application/json", authorization: `Bearer ${key}`, "x-forwarded-for": "203.0.113.9" };
    if (failUpstream) headers["x-mock-fail"] = String(failUpstream);
    return new Request("http://localhost/v1/chat/completions", {
      method: "POST",
      headers,
      body: JSON.stringify({ model, stream, messages: [{ role: "user", content: "hi" }] }),
    });
  }

  async function chat(opts = {}) {
    const handler = await chatHandler();
    const res = await handler(request(opts));
    // Drain so streaming usage/finalisation callbacks run exactly as for a real client.
    await res.text();
    return res.status;
  }

  /** Move the wall clock forward (Date only; real timers keep running). */
  async function advance(ms) {
    const { vi } = await import("vitest");
    if (!advance.fake) {
      vi.useFakeTimers({ toFake: ["Date"], now: Date.now() });
      advance.fake = true;
    }
    vi.setSystemTime(Date.now() + ms);
  }

  async function restoreClock() {
    const { vi } = await import("vitest");
    if (advance.fake) { vi.useRealTimers(); advance.fake = false; }
  }

  async function updateSettings(patch) {
    const { db } = await load();
    await db.updateSettings(patch);
    await settle();
  }

  /** Run `fn`, let background persistence settle, return D1 (+ DO) counters for the window. */
  async function measure(label, fn, { divisor = 1, flushAnalytics = false } = {}) {
    await settle();
    d1.reset();
    holder.doCounter?.reset();
    const t0 = performance.now();
    const outcome = await fn();
    await settle();
    const immediate = d1.snapshot();
    const durable = holder.doCounter ? holder.doCounter.snapshot() : null;
    const queries = d1.log.slice();
    let deferred = null;
    let deferredDurable = null;
    if (flushAnalytics) {
      const { usageRepo } = await load();
      d1.reset();
      holder.doCounter?.reset();
      await usageRepo.flushAnalyticsBuffer();
      // Coalesced outcome/metric reports are sent by their own batch flush.
      const client = await import("@/lib/coordinator/client.js");
      await client.flushCoordinatorBatches?.();
      await settle();
      deferred = d1.snapshot();
      deferredDurable = holder.doCounter ? holder.doCounter.snapshot() : null;
    }
    return { label, outcome, divisor, ms: performance.now() - t0, immediate, deferred, durable, deferredDurable, queries };
  }

  async function close() {
    await settle();
    if (mock) await mock.close();
    mock = null;
  }

  return { d1, seed, coldStart, chat, request, measure, settle, close, load, chatHandler, ensureMock, sizes, advance, restoreClock, updateSettings };
}

export const perRequest = (m, field) => (m.immediate[field] / m.divisor);
