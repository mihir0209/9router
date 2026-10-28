#!/usr/bin/env node
// Multi-process scaling benchmark: N copies of the Node build sharing one DATA_DIR behind a local L4
// round-robin balancer (scripts/bench/balancer.mjs), a mock upstream, and several pinned load-generator
// processes (scripts/bench/load-worker.mjs).
//
//   node scripts/bench/scaling.mjs --servers 4 --src ~/.9router/db/data.sqlite --out docs/benchmarks/runs/scaling/n4 \
//        [--concurrency 16,64,256,1024] [--repeats 3] [--duration 8] [--warmup 2] \
//        [--open-rates 1200,2000] [--open-duration 15] [--gens 4]
//   node scripts/bench/scaling.mjs --probe --servers 4 --src ... --out docs/benchmarks/runs/scaling/probe
//
// CPU layout (16 logical CPUs, SMT siblings are (2k, 2k+1)): server i -> CPUs 2i,2i+1 (one full physical core,
// so its GC/libuv helper threads do not preempt the event loop thread), mock -> 8, balancer -> 10, generators ->
// 12-15. N <= 4 keeps servers and infrastructure on disjoint cores. N = 8 cannot: servers 4-7 then take cores
// 4-7, which are shared with the mock, balancer and generators (16 threads for 8 servers + 7 helper processes).
// Everything runs in one process tree (the sandbox may give each shell command its own /tmp and network).
import { spawn, spawnSync, execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { DatabaseSync } from "node:sqlite";
import { performance } from "node:perf_hooks";
import { summarize } from "./load.mjs";

const root = path.resolve(path.dirname(new URL(import.meta.url).pathname), "../..");
const argv = process.argv.slice(2);
const arg = (name, fallback) => { const i = argv.indexOf(`--${name}`); return i >= 0 ? argv[i + 1] : fallback; };
const flag = (name) => argv.includes(`--${name}`);
const list = (v) => String(v).split(",").map((s) => s.trim()).filter(Boolean);

const N = Number(arg("servers", 1));
const probeName = arg("probe", null); // rate | conc | revoke (one per invocation: a 429 burst makes the server auto-block 127.0.0.1)
const probe = Boolean(probeName);
const src = arg("src", null);
const outDir = path.resolve(arg("out", path.join(root, "docs/benchmarks/runs/scaling", `n${N}`)));
const concurrencies = list(arg("concurrency", "16,64,256,1024")).map(Number);
const durationMs = Number(arg("duration", 8)) * 1000;
const warmupMs = Number(arg("warmup", 2)) * 1000;
const repeats = Number(arg("repeats", 3));
const openRates = list(arg("open-rates", String(`${250 * N},${400 * N}`))).map(Number).filter(Boolean);
const openDurationMs = Number(arg("open-duration", 15)) * 1000;
const GENS = Number(arg("gens", 4));
const separateData = flag("separate-data"); // A/B: every server gets its own copy of the DATA_DIR instead of sharing one
const direct = flag("direct"); // N=1 only: skip the balancer, load goes straight to the server (A/B for the balancer hop)
const lbPort = direct ? 20301 : 20127;
const mockPort = 20199;
const basePort = 20301;
const serverCpu = (i) => `${2 * i},${2 * i + 1}`; // one full physical core (both SMT threads) per server
const MOCK_CPU = 8;
const LB_CPU = 10;
const genCpu = (i) => [12, 13, 14, 15, 9, 11][i];

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "9r-scale-"));
const dataDir = path.join(tmp, "data");
const homeDir = path.join(tmp, "home");
fs.mkdirSync(homeDir, { recursive: true });
fs.mkdirSync(outDir, { recursive: true });

const children = new Set();
function cleanup() {
  for (const c of children) { try { c.kill("SIGKILL"); } catch { /* gone */ } }
  fs.rmSync(tmp, { recursive: true, force: true });
}
process.on("exit", cleanup);
process.on("SIGINT", () => process.exit(130));
process.on("SIGTERM", () => process.exit(143));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function pinned(cpu, command, args, options) {
  const c = spawn("taskset", ["-c", String(cpu), command, ...args], options);
  children.add(c);
  c.once("exit", () => children.delete(c));
  return c;
}

// ---- /proc sampling (per process) --------------------------------------------------------------------
const CLK_TCK = Number(execFileSync("getconf", ["CLK_TCK"]).toString()) || 100;
function descendants(pid) {
  const out = [pid];
  for (const entry of fs.readdirSync("/proc")) {
    if (!/^\d+$/.test(entry)) continue;
    try {
      const stat = fs.readFileSync(`/proc/${entry}/stat`, "utf8");
      if (Number(stat.slice(stat.lastIndexOf(")") + 2).split(" ")[1]) === pid) out.push(...descendants(Number(entry)));
    } catch { /* vanished */ }
  }
  return out;
}
function readProc(pid) {
  let ticks = 0, rssKb = 0;
  for (const p of descendants(pid)) {
    try {
      const f = fs.readFileSync(`/proc/${p}/stat`, "utf8");
      const g = f.slice(f.lastIndexOf(")") + 2).split(" ");
      ticks += Number(g[11]) + Number(g[12]);
      rssKb += Number(fs.readFileSync(`/proc/${p}/statm`, "utf8").split(" ")[1]) * 4;
    } catch { /* gone */ }
  }
  return { ticks, rssKb };
}
function startSampler(pids) {
  const first = pids.map(readProc);
  const t0 = performance.now();
  return () => {
    const sec = (performance.now() - t0) / 1000;
    const last = pids.map(readProc);
    return pids.map((_, i) => ({ cpuCores: (last[i].ticks - first[i].ticks) / CLK_TCK / sec, rssMb: last[i].rssKb / 1024 }));
  };
}

// ---- boot -------------------------------------------------------------------------------------------------
console.log(`[scale] N=${N} tmp=${tmp} out=${outDir} probe=${probe}`);
const setupArgs = ["scripts/bench/setup-bench-db.mjs", "--out", dataDir, "--mock-url", `http://127.0.0.1:${mockPort}/v1`];
if (src) setupArgs.push("--src", src);
const setup = spawnSync("node", setupArgs, { cwd: root, encoding: "utf8" });
if (setup.status !== 0) { console.error(setup.stderr || setup.stdout); process.exit(1); }
const dbInfo = JSON.parse(setup.stdout);

// Extra keys for the correctness probes: copy the plain key row with different limits.
function addProbeKeys() {
  const db = new DatabaseSync(path.join(dataDir, "db", "data.sqlite"));
  const base = db.prepare("SELECT * FROM apiKeys WHERE id = 'bench-plain'").get();
  const mk = (id, extra) => {
    const row = { ...base, id, name: id, key: `sk-bench-${id}-${Math.random().toString(16).slice(2, 14)}`, ...extra };
    const cols = Object.keys(row);
    db.prepare(`INSERT INTO apiKeys(${cols.join(",")}) VALUES(${cols.map(() => "?").join(",")})`).run(...cols.map((c) => row[c]));
    return row.key;
  };
  const keys = {
    rate: mk("probe-rate", { rateLimitRequests: 100, rateLimitWindowSeconds: 600 }),
    conc: mk("probe-conc", { maxConcurrentRequests: 4 }),
    revoke: mk("probe-revoke", {}),
  };
  db.close();
  return keys;
}
const probeKeys = probe ? addProbeKeys() : null;

let mock = null;
function startMock(delayMs) {
  mock = pinned(MOCK_CPU, "node", ["scripts/bench/mock-upstream.mjs"], {
    cwd: root,
    env: { ...process.env, MOCK_PORT: String(mockPort), MOCK_DELAY_MS: String(delayMs), MOCK_SSE_CHUNKS: "8" },
    stdio: ["ignore", "ignore", "ignore"],
  });
  return mock;
}
async function stopMock() {
  if (!mock) return;
  const exited = new Promise((r) => mock.once("exit", r));
  mock.kill("SIGKILL");
  await exited;
  mock = null;
}
startMock(0);

const serverEnv = {
  ...process.env, HOME: homeDir, DATA_DIR: dataDir, HOSTNAME: "127.0.0.1", NODE_ENV: "production",
  MODELS_LIST_SOURCE: "cached",
  HTTP_PROXY: "http://127.0.0.1:9", HTTPS_PROXY: "http://127.0.0.1:9", NO_PROXY: "127.0.0.1,localhost",
};
delete serverEnv.NINEROUTER_RUNTIME;
delete serverEnv.NODE_USE_ENV_PROXY;
for (const k of ["http_proxy", "https_proxy", "all_proxy", "ALL_PROXY"]) delete serverEnv[k];

const servers = [];
const serverLogs = [];
const dataDirFor = (i) => {
  if (!separateData || i === 0) return dataDir;
  const copy = path.join(tmp, `data-${i}`);
  fs.cpSync(dataDir, copy, { recursive: true });
  return copy;
};
async function waitHealthy(port, ms = 40_000) {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    try { if ((await fetch(`http://127.0.0.1:${port}/api/health`)).ok) return; } catch { /* not yet */ }
    await sleep(250);
  }
  console.error(serverLogs.map((f) => fs.readFileSync(f, "utf8").slice(-1500)).join("\n---\n"));
  throw new Error(`port ${port} did not become healthy`);
}
async function startServers() {
  for (let i = 0; i < N; i++) {
    const logFile = path.join(tmp, `server-${i}.log`);
    serverLogs.push(logFile);
    const fd = fs.openSync(logFile, "a");
    const port = basePort + i;
    const s = pinned(serverCpu(i), "node", ["--disable-warning=MODULE_TYPELESS_PACKAGE_JSON", "custom-server.js", "--port", String(port)], {
      cwd: root, env: { ...serverEnv, PORT: String(port), DATA_DIR: dataDirFor(i) }, stdio: ["ignore", fd, fd],
    });
    servers.push(s);
    // First process alone (it creates/migrates nothing here, the DB is already complete), then the rest.
    if (i === 0) await waitHealthy(port);
  }
  await Promise.all(servers.map((_, i) => waitHealthy(basePort + i)));
}
let lb = null;
let lbLog = "";
function startLb() {
  const backends = Array.from({ length: N }, (_, i) => basePort + i).join(",");
  lb = pinned(LB_CPU, "node", ["scripts/bench/balancer.mjs", "--listen", String(lbPort), "--backends", backends], { cwd: root, stdio: ["ignore", "pipe", "inherit"] });
  lb.stdout.on("data", (d) => { lbLog += d; });
}
async function waitLb() {
  const deadline = Date.now() + 10_000;
  while (Date.now() < deadline) { try { if ((await fetch(`http://127.0.0.1:${lbPort}/api/health`)).ok) return; } catch { /* */ } await sleep(100); }
  throw new Error("balancer not up");
}

// ---- load: several generator processes ---------------------------------------------------------------------
const auth = (key) => ({ authorization: `Bearer ${key}` });
const json = { "content-type": "application/json" };
const chatBody = (stream = false) => JSON.stringify({ model: "mock/mock-model", stream, messages: [{ role: "user", content: "hello" }] });
const chatSpec = (key) => ({ path: "/v1/chat/completions", method: "POST", headers: { ...json, ...auth(key) }, body: chatBody(false) });

function runGens(jobBase, perGen) {
  return Promise.all(Array.from({ length: GENS }, (_, g) => new Promise((resolve, reject) => {
    const out = path.join(tmp, `gen-${g}-${Date.now()}.json`);
    const job = { ...jobBase, ...perGen(g), out };
    const p = pinned(genCpu(g), "node", ["scripts/bench/load-worker.mjs", JSON.stringify(job)], { cwd: root, stdio: ["ignore", "inherit", "inherit"] });
    p.once("exit", (code) => {
      if (code !== 0) return reject(new Error(`generator ${g} exited ${code}`));
      const r = JSON.parse(fs.readFileSync(out, "utf8"));
      fs.rmSync(out, { force: true });
      resolve(r);
    });
  })));
}
function mergeResults(parts, extra) {
  const samples = parts.flatMap((p) => p.samples);
  const ok = parts.reduce((a, p) => a + p.ok, 0);
  const failed = parts.reduce((a, p) => a + p.failed, 0);
  const errors = {};
  for (const p of parts) for (const [k, v] of Object.entries(p.errors)) errors[k] = (errors[k] || 0) + v;
  const merged = {
    ...extra,
    gens: parts.length,
    ok, failed, errors, errorRate: ok + failed ? failed / (ok + failed) : 0,
    dropped: parts.reduce((a, p) => a + (p.dropped || 0), 0),
    throughput: parts.reduce((a, p) => a + p.throughput, 0),
    latencyMs: summarize(samples),
    generatorSendLagP99Ms: Math.max(...parts.map((p) => p.generatorSendLagMs?.p99 ?? 0)),
    slowest: parts.flatMap((p) => p.slowest || []).sort((a, b) => b.ms - a.ms).slice(0, 5),
    connectBurstMs: Math.max(...parts.map((p) => p.connectBurst?.totalMs ?? 0)),
  };
  return { merged, samples };
}

async function sanity() {
  const res = await fetch(`http://127.0.0.1:${lbPort}/v1/chat/completions`, { method: "POST", headers: chatSpec(dbInfo.keys.plain).headers, body: chatBody(false) });
  const body = await res.json();
  if (res.status !== 200 || !body.choices?.length) throw new Error(`sanity failed: ${res.status} ${JSON.stringify(body).slice(0, 200)}`);
}

const env = {
  label: `scaling-n${N}`,
  startedAt: new Date().toISOString(),
  commit: spawnSync("git", ["rev-parse", "--short", "HEAD"], { cwd: root, encoding: "utf8" }).stdout.trim(),
  node: process.version, platform: `${os.platform()} ${os.release()} ${os.arch()}`,
  cpuModel: os.cpus()[0]?.model, logicalCpus: os.cpus().length, totalMemMb: Math.round(os.totalmem() / 1048576),
  loadAvgAtStart: os.loadavg(),
  buildVariant: fs.existsSync(path.join(root, ".next/standalone/.9router-build-variant")) ? fs.readFileSync(path.join(root, ".next/standalone/.9router-build-variant"), "utf8").trim() : "unknown",
  separateData,
  servers: N, serverCpus: Array.from({ length: N }, (_, i) => serverCpu(i)), mockCpu: MOCK_CPU, balancerCpu: LB_CPU,
  generatorCpus: Array.from({ length: GENS }, (_, g) => genCpu(g)),
  config: { concurrencies, durationMs, warmupMs, repeats, openRates, openDurationMs, gens: GENS },
  db: dbInfo.counts,
};
fs.writeFileSync(path.join(outDir, "env.json"), JSON.stringify(env, null, 2));

await startServers();
if (!direct) { startLb(); await waitLb(); }
await sanity();

const spec = chatSpec(dbInfo.keys.plain);
const pids = servers.map((s) => s.pid);
const closed = [];
const open = [];
const pooled = new Map();

async function warm() {
  await runGens({ mode: "closed", origin: `http://127.0.0.1:${lbPort}`, spec, durationMs: 4000, warmupMs: 0 }, () => ({ concurrency: Math.max(4, 4 * N) }));
}

if (!probe) {
  await warm();
  console.log(`[scale] ${N} server(s) warm`);
  let best = 0;
  for (const c of concurrencies) {
    const perGen = Math.max(1, Math.ceil(c / GENS));
    const total = perGen * GENS;
    const expectedWms = best > 0 ? (total / best) * 1000 : 0;
    const stepWarm = Math.min(15_000, Math.max(warmupMs, 3 * expectedWms));
    const stepDur = Math.min(24_000, Math.max(durationMs, 4 * expectedWms));
    for (let run = 1; run <= repeats; run++) {
      await sleep(500);
      const lbPid = lb ? lb.pid : process.pid;
      const stop = startSampler([...pids, lbPid]);
      const parts = await runGens({ mode: "closed", origin: `http://127.0.0.1:${lbPort}`, spec, durationMs: stepDur, warmupMs: stepWarm }, () => ({ concurrency: perGen }));
      const use = stop();
      const { merged, samples } = mergeResults(parts, { mode: "closed", concurrency: total, run, durationMs: stepDur, warmupMs: stepWarm });
      merged.servers = use.slice(0, N);
      merged.serverCpuTotal = use.slice(0, N).reduce((a, u) => a + u.cpuCores, 0);
      merged.balancer = use[N];
      merged.valid = merged.errorRate <= 0.001 && merged.ok > 0;
      best = Math.max(best, merged.throughput);
      const key = `${total}`;
      if (!pooled.has(key)) pooled.set(key, []);
      for (const v of samples) pooled.get(key).push(v);
      closed.push(merged);
      const l = merged.latencyMs;
      console.log(`[closed] N=${N} c=${String(total).padStart(4)} run=${run} ${merged.throughput.toFixed(0).padStart(6)} req/s p50=${l.p50?.toFixed(2)} p99=${l.p99?.toFixed(2)} max=${l.max?.toFixed(1)} err=${(merged.errorRate * 100).toFixed(3)}% srvCPU=${merged.serverCpuTotal.toFixed(2)} (per-proc ${merged.servers.map((u) => u.cpuCores.toFixed(2)).join("/")}) lbCPU=${merged.balancer.cpuCores.toFixed(2)}${merged.valid ? "" : " INVALID"}`);
      fs.writeFileSync(path.join(outDir, "closed.json"), JSON.stringify(closed));
      fs.writeFileSync(path.join(outDir, "pooled.json"), JSON.stringify([...pooled].map(([k, v]) => ({ key: k, ...summarize(v) }))));
    }
  }
  for (const rate of openRates) {
    const perGenRate = rate / GENS;
    for (let run = 1; run <= repeats; run++) {
      await sleep(1000);
      const stop = startSampler([...pids, lb ? lb.pid : process.pid]);
      const parts = await runGens({ mode: "open", origin: `http://127.0.0.1:${lbPort}`, spec, durationMs: openDurationMs, warmupMs, maxInflight: 600 }, () => ({ rate: perGenRate }));
      const use = stop();
      const { merged, samples } = mergeResults(parts, { mode: "open", targetRate: rate, run, durationMs: openDurationMs, warmupMs });
      merged.servers = use.slice(0, N);
      merged.serverCpuTotal = use.slice(0, N).reduce((a, u) => a + u.cpuCores, 0);
      merged.balancer = use[N];
      merged.valid = merged.errorRate <= 0.001 && merged.ok > 0;
      const key = `open:${rate}`;
      if (!pooled.has(key)) pooled.set(key, []);
      for (const v of samples) pooled.get(key).push(v);
      open.push(merged);
      const l = merged.latencyMs;
      console.log(`[open]   N=${N} rate=${String(rate).padStart(5)} run=${run} got ${merged.throughput.toFixed(0)} req/s p50=${l.p50?.toFixed(2)} p99=${l.p99?.toFixed(2)} p99.9=${l.p999?.toFixed(2)} max=${l.max?.toFixed(1)} err=${(merged.errorRate * 100).toFixed(3)}% dropped=${merged.dropped} srvCPU=${merged.serverCpuTotal.toFixed(2)} lbCPU=${merged.balancer.cpuCores.toFixed(2)}${merged.valid ? "" : " INVALID"}`);
      fs.writeFileSync(path.join(outDir, "open.json"), JSON.stringify(open));
      fs.writeFileSync(path.join(outDir, "pooled.json"), JSON.stringify([...pooled].map(([k, v]) => ({ key: k, ...summarize(v) }))));
    }
  }
} else {
  // ---- correctness probes: what is per-process state, what is shared through SQLite ----------------------
  const origin = `http://127.0.0.1:${lbPort}`;
  const { Pool } = await import("undici");
  // N*k separate keep-alive connections, one request each, so every backend gets the same share.
  async function burst(key, count, { concurrent = true } = {}) {
    const pools = Array.from({ length: count }, () => new Pool(origin, { connections: 1, pipelining: 1 }));
    const one = async (pool) => {
      const res = await pool.request({ path: "/v1/chat/completions", method: "POST", headers: chatSpec(key).headers, body: chatBody(false) });
      const text = await res.body.text();
      return { status: res.statusCode, text: text.slice(0, 120) };
    };
    let results;
    if (concurrent) results = await Promise.all(pools.map(one));
    else { results = []; for (const p of pools) results.push(await one(p)); }
    await Promise.all(pools.map((p) => p.close()));
    const by = {};
    for (const r of results) by[r.status] = (by[r.status] || 0) + 1;
    return { by, sample429: results.find((r) => r.status === 429)?.text };
  }
  const report = { servers: N, probes: {} };

  if (probeName === "rate") {
  // A. rate limit: 100 requests per 600 s configured for the key
  report.probes.rateLimit = { configured: 100, windowSeconds: 600, sent: 100 * N + 40, ...(await burst(probeKeys.rate, 100 * N + 40, { concurrent: false })) };
  console.log("[probe] rate limit", JSON.stringify(report.probes.rateLimit));
  }
  if (probeName === "conc") {

  // B. concurrent-request cap: 4 in flight, upstream 300 ms
  await stopMock(); startMock(300); await sleep(500);
  report.probes.concurrencyCap = { configured: 4, sent: 8 * N, ...(await burst(probeKeys.conc, 8 * N)) };
  console.log("[probe] concurrency cap", JSON.stringify(report.probes.concurrencyCap));
  await stopMock(); startMock(0); await sleep(500);
  }
  if (probeName === "revoke") {
  // C. key revocation written by another process (direct SQLite write, no in-process cache invalidation)
  const before = await burst(probeKeys.revoke, 4 * N);
  const dbw = new DatabaseSync(path.join(dataDir, "db", "data.sqlite"));
  dbw.exec("PRAGMA busy_timeout = 5000");
  dbw.prepare("UPDATE apiKeys SET isActive = 0 WHERE id = 'probe-revoke'").run();
  dbw.close();
  const checks = [];
  for (const wait of [0, 2000, 5000]) {
    await sleep(wait);
    checks.push({ afterMs: wait, ...(await burst(probeKeys.revoke, 4 * N, { concurrent: false })) });
  }
  report.probes.revocationFromOtherWriter = { before, checks };
  console.log("[probe] revocation", JSON.stringify(report.probes.revocationFromOtherWriter));
  }

  if (probeName === "usage") {
    // E. usage persistence under N writers: requests processed vs usage rows that reached the database
    const done0 = () => serverLogs.map((f) => (fs.readFileSync(f, "utf8").match(/📊 DONE/g) || []).length).reduce((a, b) => a + b, 0);
    const doneBefore = done0();
    const parts = await runGens({ mode: "closed", origin, spec, durationMs: 8000, warmupMs: 0 }, () => ({ concurrency: 64 }));
    await sleep(15_000); // let the analytics buffers flush
    const dbu = new DatabaseSync(path.join(dataDir, "db", "data.sqlite"), { readOnly: true });
    const text = serverLogs.map((f) => fs.readFileSync(f, "utf8")).join("\n");
    report.probes.usagePersistence = {
      requestsProcessed: done0() - doneBefore,
      requestsAnsweredOk: parts.reduce((a, p) => a + p.ok, 0),
      usageHistoryRows: dbu.prepare("SELECT COUNT(*) AS n FROM usageHistory").get().n,
      totalRequestsLifetime: dbu.prepare("SELECT value FROM _meta WHERE key = 'totalRequestsLifetime'").get()?.value ?? null,
      dailyRequests: JSON.parse(dbu.prepare("SELECT data FROM usageDaily ORDER BY dateKey DESC LIMIT 1").get()?.data || "{}").requests ?? null,
      saveFailures: (text.match(/Failed to save usage stats/g) || []).length,
      databaseIsLockedLines: (text.match(/database is locked/g) || []).length,
    };
    dbu.close();
    console.log("[probe] usage persistence", JSON.stringify(report.probes.usagePersistence));
  }
  // D. database health after a short run
  if (probeName !== "usage") await warm();
  const dbr = new DatabaseSync(path.join(dataDir, "db", "data.sqlite"), { readOnly: true });
  report.probes.database = {
    integrity: dbr.prepare("PRAGMA integrity_check").get(),
    journalMode: dbr.prepare("PRAGMA journal_mode").get(),
    tables: Object.fromEntries(["usageHistory", "usageDaily", "requestDetails"].map((t) => { try { return [t, dbr.prepare(`SELECT COUNT(*) AS n FROM ${t}`).get().n]; } catch { return [t, "n/a"]; } })),
  };
  dbr.close();
  const logText = serverLogs.map((f) => fs.readFileSync(f, "utf8")).join("\n");
  report.probes.database.logErrors = {
    sqliteBusyOrLocked: (logText.match(/SQLITE_BUSY|database is locked|database table is locked/gi) || []).length,
    errorLines: (logText.match(/\bERROR\b|\[error\]/gi) || []).length,
    sampleErrorLines: logText.split("\n").filter((l) => /SQLITE_BUSY|locked|ERROR/i.test(l)).slice(0, 8),
  };
  fs.writeFileSync(path.join(outDir, `probe-${probeName}.json`), JSON.stringify(report, null, 2));
  console.log("[probe] database", JSON.stringify(report.probes.database));
}

const logText = serverLogs.map((f) => fs.readFileSync(f, "utf8")).join("\n");
const lbStats = !lb ? null : await new Promise((resolve) => {
  let done = false;
  lb.once("exit", () => { if (!done) { done = true; try { resolve(JSON.parse(lbLog.trim().split("\n").pop())); } catch { resolve(null); } } });
  lb.kill("SIGTERM");
  setTimeout(() => { if (!done) { done = true; resolve(null); } }, 3000);
});
env.finishedAt = new Date().toISOString();
env.balancerConnectionsPerBackend = lbStats?.connectionsPerBackend ?? null;
env.serverLogBusyOrLockedLines = (logText.match(/SQLITE_BUSY|database is locked/gi) || []).length;
env.serverDoneLinesPerProcess = serverLogs.map((f) => (fs.readFileSync(f, "utf8").match(/📊 DONE/g) || []).length);
env.serverDriverLines = [...new Set(logText.split("\n").filter((l) => l.includes("[DB] Driver")))];
fs.writeFileSync(path.join(outDir, "env.json"), JSON.stringify(env, null, 2));
fs.writeFileSync(path.join(outDir, "server0.log.tail.txt"), fs.readFileSync(serverLogs[0], "utf8").split("\n").slice(-40).join("\n"));
console.log(`[scale] done -> ${outDir}`);
process.exit(0);
