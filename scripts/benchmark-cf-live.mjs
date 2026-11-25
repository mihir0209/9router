#!/usr/bin/env node

/**
 * 9Router Live Cloudflare Workers + EdgeWasm Benchmark Suite
 *
 * Benchmarks:
 * 1. Memory overhead & OpenNext Cloudflare bundle sizing (.open-next/worker.js + handler + .wasm).
 * 2. EdgeWasm direct scalar fast-path & routing compute vs Pure JS.
 * 3. Pure JS GatewayCoordinator vs EdgeWasm-backed GatewayCoordinator (CompiledWasm path)
 *    across realistic multi-tenant payloads (rate limits, concurrency leases,
 *    EWMA outcome scoring, budget reservations, lock checks, fast route lookup)
 *    including DO storage read/write savings.
 * 4. Live local Cloudflare Workers (workerd):
 *    - Real workerd SQLite Durable Objects running GatewayCoordinator (Pure JS vs CompiledWasm)
 *    - Real OpenNext Worker via `wrangler dev --config wrangler.router.toml` (/api/health & /v1/models)
 *    (Pass --no-live to skip workerd spawning.)
 */

import { register } from "node:module";
import { readFileSync, existsSync, mkdtempSync, writeFileSync, copyFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { gzipSync } from "node:zlib";
import { spawn } from "node:child_process";
import net from "node:net";

const cfWorkersHook = `
export async function resolve(specifier, context, nextResolve) {
  if (specifier === "cloudflare:workers") {
    return { shortCircuit: true, url: "cloudflare-workers-stub://DurableObject" };
  }
  return nextResolve(specifier, context);
}
export async function load(url, context, nextLoad) {
  if (url === "cloudflare-workers-stub://DurableObject") {
    return {
      shortCircuit: true,
      format: "module",
      source: "export class DurableObject { constructor(ctx, env) { this.ctx = ctx; this.env = env; } }",
    };
  }
  return nextLoad(url, context);
}
`;
register("data:text/javascript," + encodeURIComponent(cfWorkersHook), import.meta.url);

const { GatewayCoordinator } = await import("../src/cloudflare/gateway-coordinator.js");
const { EdgeWasm } = await import("../src/cloudflare/edge-wasm.js");

const root = process.cwd();
const wasmPath = join(root, "src", "cloudflare", "ninerouter_edge_wasm.wasm");
const workerPath = join(root, ".open-next", "worker.js");
const serverHandlerPath = join(root, ".open-next", "server-functions", "default", "handler.mjs");
const middlewareHandlerPath = join(root, ".open-next", "middleware", "handler.mjs");
const CF_CPU_BUDGET_MICROS = 10_000.0; // 10ms Cloudflare Workers Free Tier CPU limit
const skipLive = process.argv.includes("--no-live");

// Realistic DO storage stand-in (clones stored objects just like DO SQLite KV).
function createBenchStorage() {
  const data = new Map();
  let reads = 0;
  let writes = 0;
  return {
    data,
    stats() {
      return { reads, writes };
    },
    resetStats() {
      reads = 0;
      writes = 0;
    },
    async get(key) {
      reads += 1;
      return data.has(key) ? structuredClone(data.get(key)) : undefined;
    },
    async put(key, value) {
      writes += 1;
      data.set(key, structuredClone(value));
    },
    async delete(key) {
      writes += 1;
      return data.delete(key);
    },
    async list({ prefix = "" } = {}) {
      const out = new Map();
      for (const [k, v] of data) {
        if (k.startsWith(prefix)) out.set(k, structuredClone(v));
      }
      reads += out.size;
      return out;
    },
  };
}

function summarizeNs(timingsNs) {
  timingsNs.sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
  const n = timingsNs.length;
  const totalNs = timingsNs.reduce((acc, v) => acc + v, 0n);
  const meanMicros = Number(totalNs) / n / 1000.0;
  const p50Micros = Number(timingsNs[Math.floor(n * 0.5)]) / 1000.0;
  const p95Micros = Number(timingsNs[Math.floor(n * 0.95)]) / 1000.0;
  const p99Micros = Number(timingsNs[Math.floor(n * 0.99)]) / 1000.0;
  const opsPerSec = meanMicros > 0 ? Math.round(1_000_000.0 / meanMicros) : 0;
  const budgetPct = (meanMicros / CF_CPU_BUDGET_MICROS) * 100.0;
  return { meanMicros, p50Micros, p95Micros, p99Micros, opsPerSec, budgetPct };
}

async function benchAsync(iterations, warmup, fn) {
  for (let i = 0; i < warmup; i++) await fn(i);
  const timings = new Array(iterations);
  for (let i = 0; i < iterations; i++) {
    const t0 = process.hrtime.bigint();
    await fn(i);
    timings[i] = process.hrtime.bigint() - t0;
  }
  return summarizeNs(timings);
}

function benchSync(iterations, warmup, fn) {
  for (let i = 0; i < warmup; i++) fn(i);
  const timings = new Array(iterations);
  for (let i = 0; i < iterations; i++) {
    const t0 = process.hrtime.bigint();
    fn(i);
    timings[i] = process.hrtime.bigint() - t0;
  }
  return summarizeNs(timings);
}

function getFreePort() {
  return new Promise((resolve, reject) => {
    const srv = net.createServer();
    srv.unref();
    srv.on("error", reject);
    srv.listen(0, "127.0.0.1", () => {
      const { port } = srv.address();
      srv.close(() => resolve(port));
    });
  });
}

console.log("================================================================================");
console.log("9ROUTER CLOUDFLARE LIVE EDGE & GATEWAYCOORDINATOR BENCHMARK");
console.log("================================================================================");

if (!existsSync(wasmPath)) {
  console.error(`Missing WASM binary at ${wasmPath}`);
  process.exit(1);
}

const wasmBytes = readFileSync(wasmPath);
const wasmGzip = gzipSync(wasmBytes);
const compileStart = process.hrtime.bigint();
const wasmModule = await WebAssembly.compile(wasmBytes);
const compileMicros = Number(process.hrtime.bigint() - compileStart) / 1000.0;

console.log(`WASM Binary:      ${wasmPath}`);
console.log(`WASM Size:        ${(wasmBytes.length / 1024).toFixed(2)} KiB raw / ${(wasmGzip.length / 1024).toFixed(2)} KiB gzip`);
console.log(`WASM Compile:     ${compileMicros.toFixed(1)} us (${(compileMicros / 1000).toFixed(2)} ms)`);

if (existsSync(workerPath)) {
  const entryRaw = readFileSync(workerPath);
  const entryGzip = gzipSync(entryRaw);
  console.log(`Worker Entry:     ${(entryRaw.length / 1024).toFixed(2)} KiB raw / ${(entryGzip.length / 1024).toFixed(2)} KiB gzip (.open-next/worker.js)`);
  if (existsSync(serverHandlerPath)) {
    const handlerRaw = readFileSync(serverHandlerPath);
    const handlerGzip = gzipSync(handlerRaw);
    console.log(`Server Handler:   ${(handlerRaw.length / 1024 / 1024).toFixed(2)} MiB raw / ${(handlerGzip.length / 1024 / 1024).toFixed(2)} MiB gzip`);
  }
  if (existsSync(middlewareHandlerPath)) {
    const mwRaw = readFileSync(middlewareHandlerPath);
    const mwGzip = gzipSync(mwRaw);
    console.log(`Middleware:       ${(mwRaw.length / 1024).toFixed(2)} KiB raw / ${(mwGzip.length / 1024).toFixed(2)} KiB gzip`);
  }
} else {
  console.log("Worker Bundle:    .open-next/worker.js not present (run npm run build:router)");
}

// ============================================================================
// 1. Memory Overhead Comparison
// ============================================================================
console.log("\n--- [1/4] Coordinator Memory Footprint (Pure JS vs WASM-backed) ---");

const jsStorage = createBenchStorage();
const wasmStorage = createBenchStorage();

const heapBeforeJs = process.memoryUsage().heapUsed;
const jsCoord = new GatewayCoordinator({ storage: jsStorage }, {}, { wasm: false });
const heapAfterJs = process.memoryUsage().heapUsed;

const heapBeforeWasm = process.memoryUsage().heapUsed;
const instStart = process.hrtime.bigint();
const wasmCoord = new GatewayCoordinator({ storage: wasmStorage }, {}, { wasmModule });
const instMicros = Number(process.hrtime.bigint() - instStart) / 1000.0;
const heapAfterWasm = process.memoryUsage().heapUsed;
const wasmStatus = await wasmCoord.getEdgeWasmStatus();

console.log(`Pure JS Constructor:  heap delta ~${((heapAfterJs - heapBeforeJs) / 1024).toFixed(1)} KiB`);
console.log(`WASM Instantiation:   ${instMicros.toFixed(1)} us (CompiledWasm zero-FS)`);
console.log(`WASM Linear Memory:   ${wasmStatus.memoryPages} pages (${(wasmStatus.memoryBytes / 1024).toFixed(0)} KiB), JS heap delta ~${((heapAfterWasm - heapBeforeWasm) / 1024).toFixed(1)} KiB`);

// ============================================================================
// 2. Direct EdgeWasm Scalar Fast-Path Compute vs 10ms Budget
// ============================================================================
console.log("\n--- [2/4] EdgeWasm Direct Hot-Path Compute (Zero-FS CompiledWasm) ---");

const directWasm = EdgeWasm.fromModule(wasmModule);
const TENANTS = 50;
const PROVIDERS = ["openai", "anthropic", "deepseek", "gemini", "groq"];
const CONNECTIONS = Array.from({ length: 10 }, (_, i) => `conn-${i + 1}`);
const MODELS = ["gpt-4o", "claude-3-7-sonnet", "deepseek-r1", "gemini-2.5-pro", "llama-3.3-70b"];

const routingPayload = {
  apiKeys: Array.from({ length: TENANTS }, (_, i) => ({
    id: `tenant-key-${i}`,
    key: `sk-live-tenant-${i}`,
    rate_limit: 100_000,
    rate_limit_window_ms: 60_000,
    max_concurrent: 1_000,
    allowed_models: ["*"],
    is_active: true,
  })),
  aliases: {
    "gpt4": "gpt-4o",
    "claude": "claude-3-7-sonnet",
    "smart": "combo-smart",
  },
  combos: [
    { name: "combo-smart", strategy: "round-robin", models: ["gpt-4o", "claude-3-7-sonnet", "deepseek-r1"] },
    { name: "combo-hedged", strategy: "hedged-racing", models: ["gpt-4o", "claude-3-7-sonnet"] },
  ],
  connections: PROVIDERS.flatMap((provider, pIdx) =>
    CONNECTIONS.map((cid, cIdx) => ({
      provider,
      connection_id: `${provider}-${cid}`,
      model: MODELS[pIdx],
      priority: cIdx + 1,
      weight: 100 - cIdx * 5,
      is_active: true,
      base_url: `https://api.${provider}.example/v1`,
    }))
  ),
};

directWasm.configureRouting(routingPayload);

const directSuites = [
  {
    name: "fastCheckRateLimit (scalar FFI)",
    fn: (i) => directWasm.fastCheckRateLimit(`tenant-key-${i % TENANTS}`, 100_000, 60_000, 1_000_000 + i),
  },
  {
    name: "fastBeginRequest + fastEndRequest",
    fn: (i) => {
      const r = directWasm.fastBeginRequest(`tenant-key-${i % TENANTS}`, 100, 1_000_000 + i);
      if (r.leaseId) directWasm.fastEndRequest(`tenant-key-${i % TENANTS}`, r.leaseId);
    },
  },
  {
    name: "resolveRoute (Alias + Combo + Lock)",
    fn: (i) => {
      const r = directWasm.resolveRoute({ model: "smart", apiKey: `sk-live-tenant-${i % TENANTS}`, now: 1_000_000 + i });
      if (r.lease_id) directWasm.fastEndRequest(`tenant-key-${i % TENANTS}`, r.lease_id);
    },
  },
];

for (const s of directSuites) {
  const r = benchSync(5000, 500, s.fn);
  console.log(`  ${s.name.padEnd(36)} mean ${r.meanMicros.toFixed(2).padStart(6)} us | p95 ${r.p95Micros.toFixed(2).padStart(6)} us | ${String(r.opsPerSec).padStart(7)} ops/s | ${r.budgetPct.toFixed(4)}% of 10ms`);
}

// ============================================================================
// 3. Multi-Tenant GatewayCoordinator (Pure JS vs WASM-backed)
// ============================================================================
console.log("\n--- [3/4] Pure JS vs WASM-backed GatewayCoordinator (Multi-Tenant Payloads) ---");

await jsCoord.configureRouting(routingPayload);
await wasmCoord.configureRouting(routingPayload);

for (const provider of PROVIDERS) {
  const model = MODELS[PROVIDERS.indexOf(provider)];
  await jsCoord.setLock({ provider, connectionId: `${provider}-conn-1`, model, untilMs: 9_999_999_999, status: 429, reason: "rate_limit", now: 1_000_000 });
  await wasmCoord.setLock({ provider, connectionId: `${provider}-conn-1`, model, untilMs: 9_999_999_999, status: 429, reason: "rate_limit", now: 1_000_000 });
  for (const cid of CONNECTIONS) {
    await jsCoord.recordOutcome({ provider, connectionId: `${provider}-${cid}`, model, success: true, latencyMs: 120, status: 200, costUsd: 0.002, now: 1_000_000 });
    await wasmCoord.recordOutcome({ provider, connectionId: `${provider}-${cid}`, model, success: true, latencyMs: 120, status: 200, costUsd: 0.002, now: 1_000_000 });
  }
}

const ITERS = 1000;
const WARMUP = 150;

const scenarios = [
  {
    name: "checkRateLimit (50 tenants warm)",
    run: async (coord, i) => coord.checkRateLimit({ keyId: `tenant-key-${i % TENANTS}`, limit: 100_000, windowMs: 60_000, now: 1_000_000 + i }),
  },
  {
    name: "beginRequest + endRequest (lease)",
    run: async (coord, i) => {
      const res = await coord.beginRequest({ keyId: `tenant-key-${i % TENANTS}`, maxConcurrent: 100, now: 1_000_000 + i });
      if (res.leaseId) await coord.endRequest({ keyId: `tenant-key-${i % TENANTS}`, leaseId: res.leaseId });
    },
  },
  {
    name: "recordOutcome (EWMA + cost update)",
    run: async (coord, i) => coord.recordOutcome({
      provider: PROVIDERS[i % PROVIDERS.length],
      connectionId: `${PROVIDERS[i % PROVIDERS.length]}-${CONNECTIONS[i % CONNECTIONS.length]}`,
      model: MODELS[i % MODELS.length],
      success: i % 10 !== 0,
      latencyMs: 80 + (i % 150),
      status: i % 10 !== 0 ? 200 : 503,
      costUsd: 0.0012,
      now: 1_000_000 + i,
    }),
  },
  {
    name: "recordOutcomes (batch x10 deltas)",
    run: async (coord, i) => coord.recordOutcomes({
      items: Array.from({ length: 10 }, (_, j) => ({
        provider: PROVIDERS[j % PROVIDERS.length],
        connectionId: `${PROVIDERS[j % PROVIDERS.length]}-${CONNECTIONS[j]}`,
        model: MODELS[j % MODELS.length],
        samples: 5,
        successes: 4,
        failures: 1,
        tailFailures: 0,
        latencySumMs: 420,
        latencyCount: 5,
        costUsd: 0.005,
        lastStatus: 200,
      })),
      now: 1_000_000 + i,
    }),
  },
  {
    name: "getActiveLocks (10 connections)",
    run: async (coord, i) => {
      const provider = PROVIDERS[i % PROVIDERS.length];
      return coord.getActiveLocks({
        provider,
        connectionIds: CONNECTIONS.map((c) => `${provider}-${c}`),
        model: MODELS[i % MODELS.length],
        now: 1_000_000 + i,
      });
    },
  },
  {
    name: "getScores (10 connections EWMA)",
    run: async (coord, i) => {
      const provider = PROVIDERS[i % PROVIDERS.length];
      return coord.getScores({
        provider,
        connectionIds: CONNECTIONS.map((c) => `${provider}-${c}`),
        model: MODELS[i % MODELS.length],
        now: 1_000_000 + i,
      });
    },
  },
  {
    name: "reserveBudget + releaseBudget",
    run: async (coord, i) => {
      const keyId = `tenant-key-${i % TENANTS}`;
      const requestId = `req-${i}`;
      await coord.reserveBudget({ keyId, requestId, amount: 0.05, availableBudget: 500, expiresAt: 2_000_000 + i, now: 1_000_000 + i });
      await coord.releaseBudget({ keyId, requestId, now: 1_000_000 + i });
    },
  },
  {
    name: "resolveRoute (Alias + Combo + Lock)",
    run: async (coord, i) => {
      const res = await coord.resolveRoute({
        model: "smart",
        apiKey: `sk-live-tenant-${i % TENANTS}`,
        now: 1_000_000 + i,
      });
      if (res.leaseId) await coord.endRequest({ keyId: `tenant-key-${i % TENANTS}`, leaseId: res.leaseId });
    },
  },
];

const rows = [];
for (const scenario of scenarios) {
  jsStorage.resetStats();
  const jsRes = await benchAsync(ITERS, WARMUP, (i) => scenario.run(jsCoord, i));
  const jsIo = jsStorage.stats();

  wasmStorage.resetStats();
  const wasmRes = await benchAsync(ITERS, WARMUP, (i) => scenario.run(wasmCoord, i));
  const wasmIo = wasmStorage.stats();

  rows.push({
    name: scenario.name,
    jsMean: jsRes.meanMicros,
    jsPct: jsRes.budgetPct,
    jsReads: jsIo.reads,
    wasmMean: wasmRes.meanMicros,
    wasmPct: wasmRes.budgetPct,
    wasmReads: wasmIo.reads,
    speedup: jsRes.meanMicros / wasmRes.meanMicros,
  });
}

console.log("| Operation                           | JS Mean (us) | WASM Mean (us) | Speedup | JS % 10ms | WASM % 10ms | DO Reads (JS -> WASM) |");
console.log("|-------------------------------------|--------------|----------------|---------|-----------|-------------|-----------------------|");
for (const r of rows) {
  console.log(
    `| ${r.name.padEnd(35)} | ${r.jsMean.toFixed(2).padStart(12)} | ${r.wasmMean.toFixed(2).padStart(14)} | ${(r.speedup.toFixed(2) + "x").padStart(7)} | ${(r.jsPct.toFixed(3) + "%").padStart(9)} | ${(r.wasmPct.toFixed(3) + "%").padStart(11)} | ${(r.jsReads + " -> " + r.wasmReads).padStart(21)} |`
  );
}

// ============================================================================
// 4. Live Local Cloudflare Workers (workerd) Benchmarks
// ============================================================================
console.log("\n--- [4/4] Live Local Cloudflare Workers (workerd) Probes ---");

if (skipLive) {
  console.log("Skipped live workerd probes (--no-live specified).");
} else {
  // 4a. Live workerd SQLite Durable Object probe (GatewayCoordinator Pure JS vs CompiledWasm)
  const { unstable_startWorker } = await import("wrangler");
  const probeDir = mkdtempSync(join(tmpdir(), "cf-do-bench-"));
  copyFileSync(wasmPath, join(probeDir, "ninerouter_edge_wasm.wasm"));
  copyFileSync(join(root, "src", "cloudflare", "edge-wasm.js"), join(probeDir, "edge-wasm.js"));
  copyFileSync(join(root, "src", "cloudflare", "gateway-coordinator.js"), join(probeDir, "gateway-coordinator.js"));
  writeFileSync(join(probeDir, "worker.js"), `
import wasmMod from "./ninerouter_edge_wasm.wasm";
import { GatewayCoordinator, setEdgeWasmModule } from "./gateway-coordinator.js";
setEdgeWasmModule(wasmMod);
export class GatewayCoordinatorWasm extends GatewayCoordinator {}
export class GatewayCoordinatorJs extends GatewayCoordinator {
  constructor(ctx, env) { super(ctx, env, { wasm: false }); }
}
export default {
  async fetch(req, env) {
    const url = new URL(req.url);
    const mode = url.searchParams.get("mode") || "wasm";
    const ns = mode === "js" ? env.DO_JS : env.DO_WASM;
    const stub = ns.get(ns.idFromName("bench"));
    if (url.pathname === "/status") return Response.json(await stub.getEdgeWasmStatus());
    if (url.pathname === "/init") {
      await stub.configureRouting(${JSON.stringify(routingPayload)});
      for (const p of ${JSON.stringify(PROVIDERS)}) {
        await stub.setLock({ provider: p, connectionId: p + "-conn-1", model: "gpt-4o", untilMs: 9999999999, status: 429, reason: "rl", now: 1000000 });
      }
      return Response.json({ ok: true });
    }
    if (url.pathname === "/rl") {
      return Response.json(await stub.checkRateLimit({ keyId: "tenant-key-0", limit: 100000, windowMs: 60000, now: 1000000 }));
    }
    if (url.pathname === "/locks") {
      return Response.json(await stub.getActiveLocks({ provider: "openai", connectionIds: ${JSON.stringify(CONNECTIONS.map((c) => "openai-" + c))}, model: "gpt-4o", now: 1000000 }));
    }
    if (url.pathname === "/route") {
      const r = await stub.resolveRoute({ model: "smart", apiKey: "sk-live-tenant-0", now: 1000000 });
      if (r.leaseId) await stub.endRequest({ keyId: "tenant-key-0", leaseId: r.leaseId });
      return Response.json(r);
    }
    return new Response("ok");
  }
};
`);
  writeFileSync(join(probeDir, "wrangler.toml"), `
name = "cf-do-bench"
main = "worker.js"
compatibility_date = "2024-12-30"
compatibility_flags = ["nodejs_compat"]
[[durable_objects.bindings]]
name = "DO_WASM"
class_name = "GatewayCoordinatorWasm"
[[durable_objects.bindings]]
name = "DO_JS"
class_name = "GatewayCoordinatorJs"
[[migrations]]
tag = "v1"
new_sqlite_classes = ["GatewayCoordinatorWasm", "GatewayCoordinatorJs"]
`);

  const doWorker = await unstable_startWorker({ config: join(probeDir, "wrangler.toml"), dev: { remote: false, inspectorPort: false, logLevel: "error" } });
  try {
    const wasmStat = await (await doWorker.fetch("http://localhost/status?mode=wasm")).json();
    console.log(`Live workerd SQLite DO CompiledWasm status: enabled=${wasmStat.enabled}, pages=${wasmStat.memoryPages} (${(wasmStat.memoryBytes / 1024).toFixed(0)} KiB)`);
    await doWorker.fetch("http://localhost/init?mode=js");
    await doWorker.fetch("http://localhost/init?mode=wasm");

    for (const [label, path] of [["DO checkRateLimit", "/rl"], ["DO getActiveLocks (10 conns)", "/locks"], ["DO resolveRoute (combo+lease)", "/route"]]) {
      const jsDo = await benchAsync(30, 5, async () => { await (await doWorker.fetch(`http://localhost${path}?mode=js`)).arrayBuffer(); });
      const wasmDo = await benchAsync(30, 5, async () => { await (await doWorker.fetch(`http://localhost${path}?mode=wasm`)).arrayBuffer(); });
      console.log(`  ${label.padEnd(32)} JS mean ${(jsDo.meanMicros / 1000).toFixed(2)} ms | WASM mean ${(wasmDo.meanMicros / 1000).toFixed(2)} ms (${(jsDo.meanMicros / wasmDo.meanMicros).toFixed(2)}x)`);
    }
  } finally {
    await doWorker.dispose();
    rmSync(probeDir, { recursive: true, force: true });
  }

  // 4b. Live OpenNext Router Worker HTTP benchmark via wrangler dev --config wrangler.router.toml
  if (!existsSync(workerPath)) {
    console.log("Skipped wrangler.router.toml HTTP probe (.open-next/worker.js missing).");
  } else {
    const port = await getFreePort();
    const wranglerBin = join(root, "node_modules", ".bin", "wrangler");
    const child = spawn(
      wranglerBin,
      ["dev", "--config", "wrangler.router.toml", "--local", "--port", String(port), "--inspector-port", "0"],
      {
        cwd: root,
        stdio: ["ignore", "pipe", "pipe"],
        env: { ...process.env, NO_D1_WARNING: "true" },
      }
    );

    let logs = "";
    child.stdout.on("data", (d) => { logs += d.toString(); });
    child.stderr.on("data", (d) => { logs += d.toString(); });

    try {
      const bootT0 = process.hrtime.bigint();
      let ready = false;
      let firstStatus = null;
      for (let attempt = 0; attempt < 90; attempt++) {
        if (child.exitCode !== null) break;
        try {
          const res = await fetch(`http://127.0.0.1:${port}/api/health`);
          if (res.status < 500) {
            ready = true;
            firstStatus = res.status;
            break;
          }
        } catch {
          // Not listening yet
        }
        await new Promise((r) => setTimeout(r, 250));
      }

      if (!ready) {
        console.log(`wrangler dev did not become ready (exit=${child.exitCode}). Tail:\n${logs.slice(-600)}`);
      } else {
        const bootMs = Number(process.hrtime.bigint() - bootT0) / 1e6;
        console.log(`wrangler.router.toml Isolate Boot + /api/health: ${bootMs.toFixed(1)} ms (HTTP ${firstStatus})`);

        const healthStats = await benchAsync(50, 10, async () => {
          const res = await fetch(`http://127.0.0.1:${port}/api/health`);
          await res.arrayBuffer();
        });
        console.log(`  GET /api/health (warm x50):   mean ${(healthStats.meanMicros / 1000).toFixed(2)} ms | p50 ${(healthStats.p50Micros / 1000).toFixed(2)} ms | p95 ${(healthStats.p95Micros / 1000).toFixed(2)} ms | p99 ${(healthStats.p99Micros / 1000).toFixed(2)} ms | ${healthStats.opsPerSec} req/s`);

        const guardStats = await benchAsync(50, 10, async () => {
          const res = await fetch(`http://127.0.0.1:${port}/v1/models`);
          await res.arrayBuffer();
        });
        console.log(`  GET /v1/models 401 (warm x50): mean ${(guardStats.meanMicros / 1000).toFixed(2)} ms | p50 ${(guardStats.p50Micros / 1000).toFixed(2)} ms | p95 ${(guardStats.p95Micros / 1000).toFixed(2)} ms | p99 ${(guardStats.p99Micros / 1000).toFixed(2)} ms | ${guardStats.opsPerSec} req/s`);
      }
    } finally {
      child.kill("SIGTERM");
      await new Promise((r) => setTimeout(r, 300));
      if (child.exitCode === null) child.kill("SIGKILL");
    }
  }
}
