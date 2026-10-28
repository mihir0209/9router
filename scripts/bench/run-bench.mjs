#!/usr/bin/env node
// End-to-end benchmark of the 9router Node build against a local mock upstream.
//
//   node scripts/bench/run-bench.mjs --src ~/.9router/db/data.sqlite --out docs/benchmarks/runs/node-<date> \
//        [--scenarios health,models,chat-ns-plain,chat-stream-plain,chat-ns-acl,chat-stream-acl,chat-401] \
//        [--concurrency 1,8,32,128,512,1024] [--duration 8] [--warmup 2] [--repeats 3] \
//        [--open-scenario chat-ns-plain --open-rates 200,500,1000] [--open-duration 15] \
//        [--mock-delay 0] [--server-cpus 0-3] [--mock-cpus 4-5] [--smoke]
//
// Everything happens in one process tree (setup DB -> mock upstream -> server ->
// load -> teardown) because the sandbox may give every shell command its own
// /tmp and network namespace. The server runs on a scrubbed COPY of the source
// database in a temp DATA_DIR with HOME redirected, so ~/.9router is never
// written, and with proxy env pointing at a dead port so nothing can reach a
// real provider. No credential from the source database survives the copy.
import { spawn, spawnSync, execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { performance } from "node:perf_hooks";
import { closedLoop, openLoop, summarize } from "./load.mjs";

const root = path.resolve(path.dirname(new URL(import.meta.url).pathname), "../..");
const argv = process.argv.slice(2);
const arg = (name, fallback) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 ? argv[i + 1] : fallback;
};
const flag = (name) => argv.includes(`--${name}`);
const list = (value) => String(value).split(",").map((s) => s.trim()).filter(Boolean);

const smoke = flag("smoke");
const outDir = path.resolve(arg("out", path.join(root, "docs/benchmarks/runs", `node-${new Date().toISOString().slice(0, 10)}`)));
const src = arg("src", null);
const scenarioNames = list(arg("scenarios", "health,models,chat-ns-plain,chat-stream-plain,chat-ns-acl,chat-stream-acl,chat-401"));
const concurrencies = list(arg("concurrency", smoke ? "1,8" : "1,8,32,128,512,1024")).map(Number);
const durationMs = Number(arg("duration", smoke ? 2 : 8)) * 1000;
const warmupMs = Number(arg("warmup", smoke ? 1 : 2)) * 1000;
const repeats = Number(arg("repeats", smoke ? 1 : 3));
const openScenario = arg("open-scenario", "chat-ns-plain");
const openRates = list(arg("open-rates", smoke ? "50" : "")).map(Number).filter(Boolean);
const openDurationMs = Number(arg("open-duration", smoke ? 3 : 15)) * 1000;
const mockDelay = Number(arg("mock-delay", 0));
const port = Number(arg("port", 20127));
const mockPort = Number(arg("mock-port", 20199));
const serverCpus = arg("server-cpus", "0-3");
const mockCpus = arg("mock-cpus", "4-5");
const modelsMaxConcurrency = Number(arg("models-max-concurrency", 128));
const label = arg("label", path.basename(outDir));
const serverNodeArgs = list(arg("server-node-args", "")); // e.g. --cpu-prof,--cpu-prof-dir=/tmp/prof
const logLevel = arg("log-level", null); // e.g. ERROR to silence per-request INFO lines

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "9r-bench-"));
const dataDir = path.join(tmp, "data");
const homeDir = path.join(tmp, "home");
fs.mkdirSync(homeDir, { recursive: true });

const children = [];
function cleanup() {
  for (const child of children) { try { child.kill("SIGTERM"); } catch { /* gone */ } }
  fs.rmSync(tmp, { recursive: true, force: true });
}
process.on("exit", cleanup);
process.on("SIGINT", () => process.exit(130));
process.on("SIGTERM", () => process.exit(143));

function pinned(cpus, command, args, options) {
  const hasTaskset = spawnSync("sh", ["-c", "command -v taskset"]).status === 0;
  return hasTaskset && cpus ? spawn("taskset", ["-c", cpus, command, ...args], options) : spawn(command, args, options);
}

// ---- /proc based resource sampling for a process tree -------------------------------------------
const CLK_TCK = Number(execFileSync("getconf", ["CLK_TCK"]).toString()) || 100;
function descendants(pid) {
  const out = [pid];
  for (const entry of fs.readdirSync("/proc")) {
    if (!/^\d+$/.test(entry)) continue;
    try {
      const stat = fs.readFileSync(`/proc/${entry}/stat`, "utf8");
      const ppid = Number(stat.slice(stat.lastIndexOf(")") + 2).split(" ")[1]);
      if (ppid === pid) out.push(...descendants(Number(entry)));
    } catch { /* process vanished */ }
  }
  return out;
}
function readProc(pid) {
  let ticks = 0;
  let rssKb = 0;
  let threads = 0;
  for (const p of descendants(pid)) {
    try {
      const stat = fs.readFileSync(`/proc/${p}/stat`, "utf8");
      const f = stat.slice(stat.lastIndexOf(")") + 2).split(" ");
      ticks += Number(f[11]) + Number(f[12]); // utime + stime
      threads += Number(f[17]);
      const statm = fs.readFileSync(`/proc/${p}/statm`, "utf8").split(" ");
      rssKb += Number(statm[1]) * 4;
    } catch { /* gone */ }
  }
  return { ticks, rssKb, threads };
}
function startSampler(pid, periodMs = 200) {
  const first = readProc(pid);
  const t0 = performance.now();
  let peakRssKb = first.rssKb;
  let peakThreads = first.threads;
  const rss = [];
  const timer = setInterval(() => {
    const r = readProc(pid);
    peakRssKb = Math.max(peakRssKb, r.rssKb);
    peakThreads = Math.max(peakThreads, r.threads);
    rss.push(r.rssKb);
  }, periodMs);
  return () => {
    clearInterval(timer);
    const last = readProc(pid);
    const seconds = (performance.now() - t0) / 1000;
    return {
      cpuCores: (last.ticks - first.ticks) / CLK_TCK / seconds,
      rssMbStart: first.rssKb / 1024,
      rssMbEnd: last.rssKb / 1024,
      rssMbPeak: peakRssKb / 1024,
      rssMbMean: rss.length ? rss.reduce((a, b) => a + b, 0) / rss.length / 1024 : last.rssKb / 1024,
      threadsPeak: peakThreads,
    };
  };
}

// ---- boot -------------------------------------------------------------------------------------------
console.log(`[bench] tmp=${tmp} out=${outDir}`);
const setupArgs = ["scripts/bench/setup-bench-db.mjs", "--out", dataDir, "--mock-url", `http://127.0.0.1:${mockPort}/v1`];
if (src) setupArgs.push("--src", src);
const setup = spawnSync("node", setupArgs, { cwd: root, encoding: "utf8" });
if (setup.status !== 0) { console.error(setup.stderr || setup.stdout); process.exit(1); }
const dbInfo = JSON.parse(setup.stdout);

const mock = pinned(mockCpus, "node", ["scripts/bench/mock-upstream.mjs"], {
  cwd: root,
  env: { ...process.env, MOCK_PORT: String(mockPort), MOCK_DELAY_MS: String(mockDelay), MOCK_SSE_CHUNKS: "8" },
  stdio: ["ignore", "pipe", "pipe"],
});
children.push(mock);

const serverEnv = {
  ...process.env,
  HOME: homeDir,
  DATA_DIR: dataDir,
  PORT: String(port),
  HOSTNAME: "127.0.0.1",
  NODE_ENV: "production",
  MODELS_LIST_SOURCE: "cached",
  // Nothing in the server process may reach a real provider.
  HTTP_PROXY: "http://127.0.0.1:9",
  HTTPS_PROXY: "http://127.0.0.1:9",
  NO_PROXY: "127.0.0.1,localhost",
};
delete serverEnv.NINEROUTER_RUNTIME;
if (logLevel) serverEnv.LOG_LEVEL = logLevel;

const origin = `http://127.0.0.1:${port}`;
let server = null;

// A fresh server process per scenario: no heap, cache or socket state carried over
// from a previous scenario (e.g. the memory a 1024-connection run leaves behind).
async function startServer() {
  const serverLog = fs.openSync(path.join(tmp, "server.log"), "a");
  server = pinned(serverCpus, "node", ["--disable-warning=MODULE_TYPELESS_PACKAGE_JSON", ...serverNodeArgs, "custom-server.js", "--port", String(port)], {
    cwd: root,
    env: serverEnv,
    stdio: ["ignore", serverLog, serverLog],
  });
  children.push(server);
  const deadline = Date.now() + 120_000;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`${origin}/api/health`);
      if (res.ok) return server;
    } catch { /* not up yet */ }
    await new Promise((r) => setTimeout(r, 300));
  }
  throw new Error(`server did not become healthy; log:\n${fs.readFileSync(path.join(tmp, "server.log"), "utf8").slice(-2000)}`);
}

async function stopServer() {
  if (!server) return;
  const exited = new Promise((resolve) => server.once("exit", resolve));
  server.kill("SIGTERM");
  await Promise.race([exited, new Promise((r) => setTimeout(r, 5000))]);
  try { server.kill("SIGKILL"); } catch { /* gone */ }
  await new Promise((r) => setTimeout(r, 500));
  server = null;
}

// ---- scenarios -------------------------------------------------------------------------------------
const auth = (key) => ({ authorization: `Bearer ${key}` });
const json = { "content-type": "application/json" };
const chatBody = (stream, model = "mock/mock-model") => JSON.stringify({ model, stream, messages: [{ role: "user", content: "hello" }] });
const SCENARIOS = {
  health: { path: "/api/health", method: "GET", headers: {} },
  models: { path: "/v1/models", method: "GET", headers: auth(dbInfo.keys.plain) },
  "chat-ns-plain": { path: "/v1/chat/completions", method: "POST", headers: { ...json, ...auth(dbInfo.keys.plain) }, body: chatBody(false), check: "json" },
  "chat-stream-plain": { path: "/v1/chat/completions", method: "POST", headers: { ...json, ...auth(dbInfo.keys.plain) }, body: chatBody(true), check: "sse" },
  "chat-ns-acl": { path: "/v1/chat/completions", method: "POST", headers: { ...json, ...auth(dbInfo.keys.acl) }, body: chatBody(false, "anything/else"), check: "json" },
  "chat-stream-acl": { path: "/v1/chat/completions", method: "POST", headers: { ...json, ...auth(dbInfo.keys.acl) }, body: chatBody(true, "anything/else"), check: "sse" },
  "chat-401": { path: "/v1/chat/completions", method: "POST", headers: { ...json, ...auth("sk-invalid-bench-key") }, body: chatBody(false), expectStatus: 401 },
};

async function sanity(name) {
  const spec = SCENARIOS[name];
  const res = await fetch(origin + spec.path, { method: spec.method, headers: spec.headers, body: spec.body });
  const text = await res.text();
  const expect = spec.expectStatus ?? 200;
  let detail = "";
  if (res.status !== expect) detail = `status ${res.status} != ${expect}: ${text.slice(0, 200)}`;
  else if (spec.check === "json" && !JSON.parse(text).choices?.length) detail = "no choices in response";
  else if (spec.check === "sse" && !text.includes("[DONE]")) detail = "stream did not end with [DONE]";
  else if (name === "models" && !JSON.parse(text).data?.length) detail = "empty model list";
  if (detail) throw new Error(`sanity check failed for ${name}: ${detail}`);
  return { status: res.status, bytes: text.length };
}

const env = {
  label,
  startedAt: new Date().toISOString(),
  commit: spawnSync("git", ["rev-parse", "--short", "HEAD"], { cwd: root, encoding: "utf8" }).stdout.trim(),
  node: process.version,
  platform: `${os.platform()} ${os.release()} ${os.arch()}`,
  cpuModel: os.cpus()[0]?.model,
  logicalCpus: os.cpus().length,
  totalMemMb: Math.round(os.totalmem() / 1048576),
  loadAvgAtStart: os.loadavg(),
  serverCpus, mockCpus,
  genCpus: "unpinned (shares the machine with whatever else is running)",
  buildVariant: fs.existsSync(path.join(root, ".next/standalone/.9router-build-variant")) ? fs.readFileSync(path.join(root, ".next/standalone/.9router-build-variant"), "utf8").trim() : "unknown",
  server: { command: "node custom-server.js --port " + port, dataDirCopyOf: src ? "source DB (scrubbed)" : "empty", db: dbInfo.counts },
  mock: { delayMs: mockDelay, sseChunks: 8 },
  serverLogLevel: logLevel || "INFO (default)",
  config: { scenarioNames, concurrencies, durationMs, warmupMs, repeats, openScenario, openRates, openDurationMs },
};
fs.mkdirSync(outDir, { recursive: true });
fs.writeFileSync(path.join(outDir, "env.json"), JSON.stringify(env, null, 2));

const results = { closed: [], open: [] };
// Exact percentiles over the pooled samples of all repeats of a cell (n = sum of the repeats), in
// addition to the per-run percentiles: pooling is what makes p99.9 meaningful at modest rates.
const pooledSamples = new Map();
const genCpuStart = process.cpuUsage();
let bootRssMb = null;

async function freshServerFor(name) {
  await startServer();
  await sanity(name);
  // Warm-up on this scenario's own path: JIT, route compilation, pools, caches.
  await closedLoop({ origin, spec: SCENARIOS[name], concurrency: 16, durationMs: 4000, warmupMs: 0 });
  bootRssMb = readProc(server.pid).rssKb / 1024;
  console.log(`[bench] fresh server for ${name} (pid ${server.pid}, rss ${bootRssMb.toFixed(0)} MB after warm-up)`);
}

for (const name of scenarioNames) {
  const spec = SCENARIOS[name];
  await freshServerFor(name);
  let bestLambda = 0; // best throughput seen so far in this scenario
  for (const concurrency of concurrencies) {
    if (name === "models" && concurrency > modelsMaxConcurrency) continue;
    // Closed loop at population c settles at W ~= c / lambda. The warm-up must cover several W (the
    // initial burst of c requests has to drain) and the window must span several W to be
    // representative, so both scale with the latency expected from the previous step.
    const expectedWms = bestLambda > 0 ? (concurrency / bestLambda) * 1000 : 0;
    const stepWarmupMs = Math.min(15_000, Math.max(warmupMs, 3 * expectedWms));
    const stepDurationMs = Math.min(24_000, Math.max(durationMs, 4 * expectedWms));
    for (let run = 1; run <= repeats; run++) {
      await new Promise((r) => setTimeout(r, 500));
      const stopServerSampler = startSampler(server.pid);
      const cpu0 = process.cpuUsage();
      const t0 = performance.now();
      const result = await closedLoop({ origin, spec, concurrency, durationMs: stepDurationMs, warmupMs: stepWarmupMs });
      bestLambda = Math.max(bestLambda, result.throughput);
      const cellKey = `${name}|${concurrency}`;
      if (!pooledSamples.has(cellKey)) pooledSamples.set(cellKey, []);
      for (const v of result.samples) pooledSamples.get(cellKey).push(v);
      delete result.samples;
      const wall = (performance.now() - t0) / 1000;
      const cpu = process.cpuUsage(cpu0);
      const serverUse = stopServerSampler();
      result.scenario = name;
      result.run = run;
      result.server = serverUse;
      result.generatorCpuCores = (cpu.user + cpu.system) / 1e6 / wall;
      // A run is only valid if (nearly) every request succeeded; an all-error run must never be
      // read as a latency result. The 401 scenario expects 401, so "success" there means 401.
      result.valid = result.errorRate <= 0.001 && result.ok > 0;
      if (!result.valid) console.log(`[bench] WARNING: run ${name} c=${concurrency} run=${run} INVALID (error rate ${(result.errorRate * 100).toFixed(2)}%, errors ${JSON.stringify(result.errors)})`);
      results.closed.push(result);
      const l = result.latencyMs;
      console.log(`[closed] ${name.padEnd(18)} c=${String(concurrency).padStart(4)} run=${run} ${result.throughput.toFixed(0).padStart(7)} req/s  p50=${l.p50?.toFixed(2)} p99=${l.p99?.toFixed(2)} max=${l.max?.toFixed(1)} err=${(result.errorRate * 100).toFixed(2)}% srvCPU=${serverUse.cpuCores.toFixed(2)} genCPU=${result.generatorCpuCores.toFixed(2)} rss=${serverUse.rssMbPeak.toFixed(0)}MB`);
      fs.writeFileSync(path.join(outDir, "closed.json"), JSON.stringify(results.closed));
      fs.writeFileSync(path.join(outDir, "pooled.json"), JSON.stringify([...pooledSamples].map(([k, v]) => {
        const [scenario, c] = k.split("|");
        return { scenario, concurrency: Number(c), ...summarize(v) };
      })));
    }
  }
  await stopServer();
}

if (openRates.length) {
  const spec = SCENARIOS[openScenario];
  await freshServerFor(openScenario);
  for (const rate of openRates) {
    for (let run = 1; run <= repeats; run++) {
      await new Promise((r) => setTimeout(r, 1000));
      const stopServerSampler = startSampler(server.pid);
      const cpu0 = process.cpuUsage();
      const t0 = performance.now();
      const result = await openLoop({ origin, spec, rate, durationMs: openDurationMs, warmupMs });
      const wall = (performance.now() - t0) / 1000;
      const cpu = process.cpuUsage(cpu0);
      result.scenario = openScenario;
      result.run = run;
      result.server = stopServerSampler();
      result.generatorCpuCores = (cpu.user + cpu.system) / 1e6 / wall;
      const openKey = `open:${openScenario}|${rate}`;
      if (!pooledSamples.has(openKey)) pooledSamples.set(openKey, []);
      for (const v of result.samples) pooledSamples.get(openKey).push(v);
      delete result.samples;
      result.valid = result.errorRate <= 0.001 && result.ok > 0;
      if (!result.valid) console.log(`[bench] WARNING: open run rate=${rate} run=${run} INVALID (error rate ${(result.errorRate * 100).toFixed(2)}%)`);
      results.open.push(result);
      const l = result.latencyMs;
      console.log(`[open]   ${openScenario.padEnd(18)} rate=${String(rate).padStart(5)} run=${run} got ${result.throughput.toFixed(0)} req/s p50=${l.p50?.toFixed(2)} p99=${l.p99?.toFixed(2)} p99.9=${l.p999?.toFixed(2)} max=${l.max?.toFixed(1)} err=${(result.errorRate * 100).toFixed(2)}% lagP99=${result.generatorSendLagMs.p99?.toFixed(2)}ms`);
      fs.writeFileSync(path.join(outDir, "open.json"), JSON.stringify(results.open));
      fs.writeFileSync(path.join(outDir, "pooled.json"), JSON.stringify([...pooledSamples].map(([k, v]) => {
        const [scenario, c] = k.split("|");
        return { scenario, [scenario.startsWith("open:") ? "rate" : "concurrency"]: Number(c), ...summarize(v) };
      })));
    }
  }
}

const endRss = server ? readProc(server.pid) : { rssKb: 0 };
await stopServer();
env.finishedAt = new Date().toISOString();
env.serverRssMbAtEnd = endRss.rssKb / 1024;
env.generatorCpuSecondsTotal = (process.cpuUsage(genCpuStart).user + process.cpuUsage(genCpuStart).system) / 1e6;
fs.writeFileSync(path.join(outDir, "env.json"), JSON.stringify(env, null, 2));
fs.writeFileSync(path.join(outDir, "server.log.tail.txt"), fs.readFileSync(path.join(tmp, "server.log"), "utf8").split("\n").slice(-60).join("\n"));
console.log(`[bench] done -> ${outDir}`);
cleanup();
process.exit(0);
