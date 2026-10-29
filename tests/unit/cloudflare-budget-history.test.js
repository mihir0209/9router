// Cold-isolate cost of reading a budget key's spend when usageHistory is large (I).
//
// Before: the first budget read per isolate loaded EVERY usageHistory row (all keys, all time) into
// the isolate heap. Now it loads only the rows of that key inside its budget window, and a
// ledger insert on a cold isolate cannot reuse an id of a row that was never loaded.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";

const holder = vi.hoisted(() => ({ d1: null, pending: [], doCounter: null }));
vi.mock("@opennextjs/cloudflare", () => ({
  getCloudflareContext: async () => ({
    env: { DB: holder.d1, GATEWAY_COORDINATOR: holder.doCounter?.namespace },
    ctx: { waitUntil: (p) => { holder.pending.push(p); } },
  }),
}));
vi.mock("cloudflare:workers", () => ({
  DurableObject: class { constructor(ctx, env) { this.ctx = ctx; this.env = env; } },
}));

process.env.NINEROUTER_RUNTIME = "cloudflare";
const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "9router-budgethist-"));
process.env.DATA_DIR = tempDir;
process.setMaxListeners(100);

const { createHarness, BUDGET_KEY } = await import("../helpers/d1OpsHarness.js");
const { createCountingDO } = await import("../helpers/countingDO.js");

holder.doCounter = await createCountingDO();
const harness = createHarness(holder);

const OTHER_ROWS = 4000;       // other keys, and this key's rows before the budget window
const IN_WINDOW_ROWS = 30;     // this key, inside its 720 h window, 0.5 USD each
let nextId = 1;

function insertHistory(row) {
  const id = nextId++;
  const data = JSON.stringify({ id, ...row });
  holder.d1.raw.prepare("INSERT INTO ninerouter_rows(table_name, row_key, data, updated_at) VALUES('usageHistory', ?, ?, ?)")
    .run(String(id), data, new Date().toISOString());
  return id;
}

beforeAll(async () => {
  await harness.seed();
  const now = Date.now();
  for (let i = 0; i < OTHER_ROWS; i++) {
    const byBudgetKeyOld = i % 4 === 0;
    insertHistory({
      timestamp: new Date(now - (40 + (i % 300)) * 86400000).toISOString(),   // older than the 30 day window
      provider: "budget", model: `budget-commit:old-${i}`,
      apiKey: byBudgetKeyOld ? BUDGET_KEY : `sk-other-${i % 7}`, apiKeyId: byBudgetKeyOld ? "key-budget" : `key-other-${i % 7}`,
      endpoint: "/api/key-actions/debit", promptTokens: 0, completionTokens: 0, cost: 0.25, status: "success", tokens: {}, meta: {},
    });
  }
  for (let i = 0; i < IN_WINDOW_ROWS; i++) {
    insertHistory({
      // the window opens when the key was created (just now), so in-window rows are stamped after it
      timestamp: new Date(now + (i + 1) * 1000).toISOString(),
      provider: "budget", model: `budget-commit:new-${i}`, apiKey: BUDGET_KEY, apiKeyId: "key-budget",
      endpoint: "/api/key-actions/debit", promptTokens: 0, completionTokens: 0, cost: 0.5, status: "success", tokens: {}, meta: {},
    });
  }
  // And one row of a different key inside the window, which must not count.
  insertHistory({
    timestamp: new Date(now + 5000).toISOString(), provider: "budget", model: "budget-commit:neighbour",
    apiKey: "sk-other-1", apiKeyId: "key-other-1", endpoint: "/api/key-actions/debit", promptTokens: 0, completionTokens: 0, cost: 99, status: "success", tokens: {}, meta: {},
  });
}, 120_000);

afterAll(async () => {
  await harness.close();
  fs.rmSync(tempDir, { recursive: true, force: true });
});

describe("cold budget read with a large usageHistory", () => {
  it("reads only this key's in-window rows and gets the same spend", async () => {
    await harness.coldStart();
    const { getBudgetState } = await import("@/lib/budgetService.js");
    const r = await harness.measure("cold: budget state", () => getBudgetState(BUDGET_KEY));
    const rowsRead = r.immediate.rowsRead;
    console.log(`[budget-history] cold getBudgetState: D1 rows read = ${rowsRead} (usageHistory holds ${harness.d1.rowCount("usageHistory")} rows)`);
    expect(r.outcome.used).toBeCloseTo(IN_WINDOW_ROWS * 0.5, 6);
    // Settings/apiKeys are read once (about 60 rows here); the history part must be tiny.
    expect(rowsRead).toBeLessThan(IN_WINDOW_ROWS + 120);
  }, 60_000);

  it("a full chat request with the key stays cheap on a cold isolate", async () => {
    await harness.coldStart();
    const r = await harness.measure("cold: chat budget key", () => harness.chat({ key: BUDGET_KEY }));
    console.log(`[budget-history] cold chat (budget key): D1 rows read = ${r.immediate.rowsRead}, queries = ${r.immediate.queries}`);
    expect(r.outcome).toBe(200);
    expect(r.immediate.rowsRead).toBeLessThan(IN_WINDOW_ROWS + 300);
  }, 60_000);

  it("commits on a cold isolate without reusing the id of an unloaded row", async () => {
    await harness.coldStart();
    const { debitBudget, getBudgetState } = await import("@/lib/budgetService.js");
    const before = harness.d1.rowCount("usageHistory");
    const out = await debitBudget(BUDGET_KEY, { requestId: "cold-commit-1", commitUsd: 2 });
    await harness.settle();
    expect(out.committed).toBe(true);
    expect(out.used).toBeCloseTo(IN_WINDOW_ROWS * 0.5 + 2, 6);
    expect(harness.d1.rowCount("usageHistory")).toBe(before + 1);     // appended, nothing overwritten
    // A second cold isolate sees the committed spend, and committing the same request is idempotent.
    await harness.coldStart();
    const again = await (await import("@/lib/budgetService.js")).getBudgetState(BUDGET_KEY);
    expect(again.used).toBeCloseTo(IN_WINDOW_ROWS * 0.5 + 2, 6);
    const dup = await (await import("@/lib/budgetService.js")).debitBudget(BUDGET_KEY, { requestId: "cold-commit-1", commitUsd: 2 });
    await harness.settle();
    expect(dup.committed).toBe(true);
    expect(harness.d1.rowCount("usageHistory")).toBe(before + 1);
    expect(typeof getBudgetState).toBe("function");
  }, 60_000);
});
