// Per-request D1 (and Durable Object) accounting for the Cloudflare runtime.
//
// Runs the real chat pipeline against a counting fake D1 (tests/helpers/
// countingD1.js), a counting Durable Object namespace (tests/helpers/
// countingDO.js) and a local mock upstream. Set D1_OPS_REPORT=<path> to also
// write the raw numbers as JSON (docs/benchmarks/d1-ops-*.json).
//
// The assertions are the regression budget for the "extreme" Cloudflare
// profile: the warm hot path must not touch D1 at all, and cold hydration and
// periodic refresh must stay inside the ceilings below.
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
const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "9router-d1ops-"));
process.env.DATA_DIR = tempDir;
process.setMaxListeners(100);

const { createHarness, BENCH_KEY, PINNED_KEY, BUDGET_KEY, LIMITS_KEY, RATE_KEY, CONC_KEY } = await import("../helpers/d1OpsHarness.js");
const { createCountingDO } = await import("../helpers/countingDO.js");

holder.doCounter = await createCountingDO();
const harness = createHarness(holder);
const report = { generatedAt: new Date().toISOString(), label: process.env.D1_OPS_LABEL || "run", dataset: harness.sizes, scenarios: [] };
const WARM_N = 20;

function row(m) {
  const d = m.divisor;
  const i = m.immediate;
  const f = m.deferred;
  const o = m.durable;
  const fd = m.deferredDurable;
  // Amortised = request path + the batched flush that belongs to those requests.
  const sum = (a, b, k) => (a?.[k] || 0) + (b?.[k] || 0);
  return {
    scenario: m.label,
    requests: d,
    outcome: m.outcome,
    perRequest: {
      queries: i.queries / d,
      roundTrips: i.roundTrips / d,
      rowsRead: i.rowsRead / d,
      rowsWritten: i.rowsWritten / d,
      rowsWrittenWorstCase: i.rowsWrittenWorstCase / d,
      doRpcs: o ? o.rpcs / d : 0,
      doRowsRead: o ? o.rowsRead / d : 0,
      doRowsWritten: o ? o.rowsWritten / d : 0,
    },
    amortizedPerRequest: {
      d1RowsRead: sum(i, f, "rowsRead") / d,
      d1RowsWritten: sum(i, f, "rowsWritten") / d,
      d1RowsWrittenWorstCase: sum(i, f, "rowsWrittenWorstCase") / d,
      d1Queries: sum(i, f, "queries") / d,
      doRpcs: sum(o, fd, "rpcs") / d,
      doRowsRead: sum(o, fd, "rowsRead") / d,
      doRowsWritten: sum(o, fd, "rowsWritten") / d,
    },
    immediate: i,
    deferredFlush: f,
    durableObject: o,
    deferredDurableObject: fd,
    d1Statements: m.queries.map((q) => `${q.kind} n=${q.n} ${q.sql} ${JSON.stringify(q.params)}`),
  };
}

async function scenario(label, fn, opts) {
  const m = await harness.measure(label, fn, opts);
  const r = row(m);
  report.scenarios.push(r);
  return r;
}

const byLabel = (label) => report.scenarios.find((s) => s.scenario === label);
async function loop(n, fn) {
  let last = 0;
  for (let i = 0; i < n; i++) last = await fn();
  return last;
}

async function modelsRequest(key) {
  const { GET } = await import("@/app/api/v1/models/route.js");
  const res = await GET(new Request("http://localhost/v1/models", { headers: { authorization: `Bearer ${key}` } }));
  await res.text();
  return res.status;
}
async function healthRequest() {
  const { GET } = await import("@/app/api/health/route.js");
  const res = await GET();
  await res.text();
  return res.status;
}
async function cronRequest() {
  const { POST } = await import("@/app/api/cron/flush/route.js");
  const res = await POST(new Request("http://localhost/api/cron/flush", { method: "POST" }));
  await res.text();
  return res.status;
}

beforeAll(async () => {
  await harness.seed();
}, 120_000);

afterAll(async () => {
  await harness.restoreClock();
  await harness.close();
  if (process.env.D1_OPS_REPORT) {
    fs.mkdirSync(path.dirname(process.env.D1_OPS_REPORT), { recursive: true });
    fs.writeFileSync(process.env.D1_OPS_REPORT, JSON.stringify(report, null, 2));
  }
  const f = (n) => n.toFixed(3).padStart(9);
  console.log("\nPER REQUEST, AMORTISED (request path + its batched flushes): D1 rows read / written / queries | DO rpcs / rows read / rows written");
  for (const s of report.scenarios) {
    const a = s.amortizedPerRequest;
    console.log(`  ${s.scenario.padEnd(48)} D1 r=${f(a.d1RowsRead)} w=${f(a.d1RowsWritten)} q=${f(a.d1Queries)} | DO rpc=${f(a.doRpcs)} r=${f(a.doRowsRead)} w=${f(a.doRowsWritten)}`);
  }
  fs.rmSync(tempDir, { recursive: true, force: true });
});

describe("cold isolate (first request in a fresh Worker isolate)", () => {
  it("chat non-stream", async () => {
    await harness.coldStart();
    const r = await scenario("cold: chat non-stream", () => harness.chat({ stream: false }));
    expect(r.outcome).toBe(200);
  }, 60_000);

  it("chat stream", async () => {
    await harness.coldStart();
    const r = await scenario("cold: chat stream", () => harness.chat({ stream: true }));
    expect(r.outcome).toBe(200);
  }, 60_000);

  it("chat with a budget key", async () => {
    await harness.coldStart();
    const r = await scenario("cold: chat budget key", () => harness.chat({ key: BUDGET_KEY }));
    expect(r.outcome).toBe(200);
  }, 60_000);

  it("GET /v1/models", async () => {
    await harness.coldStart();
    const r = await scenario("cold: GET /v1/models", () => modelsRequest(BENCH_KEY));
    expect(r.outcome).toBe(200);
  }, 60_000);

  it("GET /api/health", async () => {
    await harness.coldStart();
    const r = await scenario("cold: GET /api/health", () => healthRequest());
    expect(r.outcome).toBe(200);
  }, 60_000);

  it("cron flush", async () => {
    await harness.coldStart();
    const r = await scenario("cold: POST /api/cron/flush", () => cronRequest());
    expect(r.outcome).toBe(200);
  }, 60_000);
});

describe("warm isolate (per-request cost)", () => {
  it("prime", async () => {
    await harness.coldStart();
    expect(await harness.chat({ stream: false })).toBe(200);
    expect(await harness.chat({ key: BUDGET_KEY })).toBe(200);
    expect(await harness.chat({ key: LIMITS_KEY })).toBe(200);
    expect(await modelsRequest(BENCH_KEY)).toBe(200);
  }, 60_000);

  const warmCases = [
    ["chat non-stream", () => harness.chat({ stream: false }), 200],
    ["chat stream", () => harness.chat({ stream: true }), 200],
    ["chat pinned key (force pins + ACL)", () => harness.chat({ key: PINNED_KEY, model: "other/anything" }), 200],
    ["chat rate-limited key", () => harness.chat({ key: RATE_KEY }), 200],
    ["chat max-concurrent key", () => harness.chat({ key: CONC_KEY }), 200],
    ["chat token-limit key", () => harness.chat({ key: LIMITS_KEY }), 200],
    ["chat budget key", () => harness.chat({ key: BUDGET_KEY }), 200],
    ["GET /v1/models", () => modelsRequest(BENCH_KEY), 200],
    ["GET /api/health", () => healthRequest(), 200],
  ];
  for (const [name, fn, status] of warmCases) {
    it(name, async () => {
      const r = await scenario(`warm: ${name} x${WARM_N}`, () => loop(WARM_N, fn), { divisor: WARM_N, flushAnalytics: true });
      expect(r.outcome).toBe(status);
    }, 60_000);
  }

  it("invalid API key", async () => {
    const r = await scenario(`warm: invalid API key x${WARM_N}`, () => loop(WARM_N, () => harness.chat({ key: "sk-nope" })), { divisor: WARM_N, flushAnalytics: true });
    expect(r.outcome).toBe(401);
  }, 60_000);

  it("upstream error then recovery", async () => {
    const r = await scenario("warm: upstream 500 x5", () => loop(5, () => harness.chat({ model: "mock/fail-500" })), { divisor: 5, flushAnalytics: true });
    expect(r.outcome).toBeGreaterThanOrEqual(400);
  }, 60_000);
});

describe("steady state: one warm isolate for an hour of simulated time", () => {
  it("1 request per 10 s for 1 hour (360 requests)", async () => {
    await harness.coldStart();
    expect(await harness.chat({ stream: false })).toBe(200); // hydrate
    const r = await scenario("steady 1h @ 0.1 req/s (360 req)", async () => {
      let last = 0;
      for (let i = 0; i < 360; i++) {
        await harness.advance(10_000);
        last = await harness.chat({ stream: i % 2 === 0 });
      }
      return last;
    }, { divisor: 360, flushAnalytics: true });
    await harness.restoreClock();
    expect(r.outcome).toBe(200);
  }, 120_000);

  it("1 request per minute for 1 hour (60 requests)", async () => {
    await harness.coldStart();
    expect(await harness.chat({ stream: false })).toBe(200);
    const r = await scenario("steady 1h @ 1 req/min (60 req)", async () => {
      let last = 0;
      for (let i = 0; i < 60; i++) {
        await harness.advance(60_000);
        last = await harness.chat({ stream: false });
      }
      return last;
    }, { divisor: 60, flushAnalytics: true });
    await harness.restoreClock();
    expect(r.outcome).toBe(200);
  }, 120_000);
});

describe("observability on (requestDetails)", () => {
  it("per-request cost with enableObservability", async () => {
    await harness.updateSettings({ enableObservability: true });
    await harness.coldStart();
    expect(await harness.chat({ stream: false })).toBe(200);
    const r = await scenario(`warm: observability on x${WARM_N}`, () => loop(WARM_N, () => harness.chat({ stream: false })), { divisor: WARM_N, flushAnalytics: true });
    expect(r.outcome).toBe(200);
    await harness.updateSettings({ enableObservability: false });
  }, 60_000);
});

// ---------------------------------------------------------------------------
// Before / after on the same code: the "standard" profile is the previous
// per-request persistence behaviour, "extreme" is the Cloudflare default.
// ---------------------------------------------------------------------------
describe("standard (before) vs extreme (after) profile", () => {
  async function profileRun(profile, label, requests, fn) {
    process.env.NINEROUTER_D1_PROFILE = profile;
    try {
      await harness.coldStart();
      expect(await harness.chat({ stream: false })).toBe(200); // hydrate lazy tables
      return await scenario(`${label} [${profile}] x${requests}`, () => loop(requests, fn), { divisor: requests, flushAnalytics: true });
    } finally {
      delete process.env.NINEROUTER_D1_PROFILE;
    }
  }

  it("warm chat: the extreme profile removes every per-request coordinator and D1 operation", async () => {
    const before = await profileRun("standard", "warm chat non-stream", 30, () => harness.chat({ stream: false }));
    const after = await profileRun("extreme", "warm chat non-stream", 30, () => harness.chat({ stream: false }));

    // Before: ~3 Durable Object RPCs, ~7 row reads and ~4 row writes per request.
    expect(before.perRequest.doRpcs).toBeGreaterThanOrEqual(3);
    expect(before.perRequest.doRowsWritten).toBeGreaterThanOrEqual(4);
    // After, on the request path itself: nothing.
    expect(after.perRequest.doRpcs).toBe(0);
    expect(after.perRequest.doRowsRead).toBe(0);
    expect(after.perRequest.doRowsWritten).toBe(0);
    expect(after.perRequest.rowsRead).toBe(0);
    expect(after.perRequest.rowsWritten).toBe(0);
    expect(after.perRequest.queries).toBe(0);
    // Amortised including the batched flush of those 30 requests: at least 20x fewer DO row writes.
    expect(after.amortizedPerRequest.doRowsWritten * 20).toBeLessThanOrEqual(before.amortizedPerRequest.doRowsWritten);
    expect(after.amortizedPerRequest.doRpcs * 10).toBeLessThanOrEqual(before.amortizedPerRequest.doRpcs);
  }, 90_000);

  it("failure path: a locked account no longer costs a D1 write", async () => {
    // Distinct failing models: the coordinator lock from one run must not short-circuit the other.
    const before = await profileRun("standard", "warm upstream 502", 4, () => harness.chat({ model: "mock/fail-502" }));
    const after = await profileRun("extreme", "warm upstream 503", 4, () => harness.chat({ model: "mock/fail-503" }));
    expect(before.immediate.rowsWritten).toBeGreaterThanOrEqual(1);
    expect(after.immediate.rowsWritten).toBe(0);
    expect(after.immediate.queries).toBe(0);
  }, 90_000);

  it("unknown API keys cannot force D1 reads", async () => {
    const before = await profileRun("standard", "invalid API key", 20, () => harness.chat({ key: "sk-nope" }));
    const after = await profileRun("extreme", "invalid API key", 20, () => harness.chat({ key: "sk-nope" }));
    expect(before.immediate.rowsRead).toBeGreaterThanOrEqual(20);
    expect(after.immediate.rowsRead).toBe(0);
    expect(after.outcome).toBe(401);
  }, 90_000);
});

describe("extreme profile budgets (regression ceilings)", () => {
  it("cold chat hydrates a bounded number of rows and never the kv reference data", () => {
    const cold = byLabel("cold: chat non-stream");
    expect(cold.immediate.rowsRead).toBeLessThanOrEqual(130); // 5,291 custom-model rows stay in D1
    expect(cold.immediate.rowsWritten).toBe(0);
    expect(cold.immediate.queries).toBeLessThanOrEqual(5);
  });

  it("health and cron never touch D1", () => {
    for (const label of ["cold: GET /api/health", "cold: POST /api/cron/flush"]) {
      expect(byLabel(label).immediate.queries).toBe(0);
    }
  });

  it("warm request path is free of D1 and coordinator operations for plain, stream, pinned and token-limit keys", () => {
    for (const name of ["chat non-stream", "chat stream", "chat pinned key (force pins + ACL)", "chat token-limit key"]) {
      const r = byLabel(`warm: ${name} x${WARM_N}`);
      expect(r.immediate.queries, name).toBe(0);
      expect(r.durableObject.rpcs, name).toBe(0);
    }
  });

  it("opt-in per-key features cost only what they must", () => {
    const rate = byLabel(`warm: chat rate-limited key x${WARM_N}`);
    expect(rate.durableObject.rpcs).toBe(WARM_N); // one checkRateLimit each
    const conc = byLabel(`warm: chat max-concurrent key x${WARM_N}`);
    expect(conc.durableObject.rpcs).toBeLessThanOrEqual(WARM_N * 2 + 2); // begin + end lease
  });

  it("steady state stays under 0.5 D1 rows read and 0.1 rows written per request", () => {
    for (const label of ["steady 1h @ 0.1 req/s (360 req)"]) {
      const r = byLabel(label);
      expect(r.amortizedPerRequest.d1RowsRead, label).toBeLessThanOrEqual(0.5);
      expect(r.amortizedPerRequest.d1RowsWritten, label).toBeLessThanOrEqual(0.1);
    }
  });

  it("usage analytics are still recorded (one aggregate flush, 2 rows)", () => {
    const r = byLabel(`warm: chat non-stream x${WARM_N}`);
    expect(r.deferredFlush.rowsWritten).toBe(2); // usageDaily + totalRequestsLifetime
  });
});

export { byLabel };
