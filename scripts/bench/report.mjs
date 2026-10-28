#!/usr/bin/env node
// Turn the raw JSON written by run-bench.mjs into markdown tables.
//   node scripts/bench/report.mjs docs/benchmarks/runs/node-main [more run dirs...] > tables.md
//
// Per (scenario, concurrency) cell the numbers are the mean +- sample standard
// deviation across the repeated runs; every run's percentiles were themselves
// computed from that run's full sorted sample set (nearest rank).
import fs from "node:fs";
import path from "node:path";

const dirs = process.argv.slice(2);
if (!dirs.length) { console.error("usage: report.mjs <run-dir>..."); process.exit(2); }

const mean = (xs) => xs.reduce((a, b) => a + b, 0) / xs.length;
const sd = (xs) => (xs.length > 1 ? Math.sqrt(xs.reduce((a, b) => a + (b - mean(xs)) ** 2, 0) / (xs.length - 1)) : 0);
// Two-sided 95% Student-t critical values by degrees of freedom (n-1); normal approximation beyond 10.
const T95 = [NaN, 12.706, 4.303, 3.182, 2.776, 2.571, 2.447, 2.365, 2.306, 2.262, 2.228];
const ci95 = (xs) => (xs.length > 1 ? ((T95[xs.length - 1] ?? 1.96) * sd(xs)) / Math.sqrt(xs.length) : 0);
const fmt = (x, d = 2) => (x === null || x === undefined || Number.isNaN(x) ? "n/a" : x.toFixed(d));
const pm = (xs, d = 2) => (xs.length > 1 ? `${fmt(mean(xs), d)} ±${fmt(sd(xs), d)}` : fmt(xs[0], d));
const group = (rows, keyFn) => {
  const m = new Map();
  for (const r of rows) { const k = keyFn(r); if (!m.has(k)) m.set(k, []); m.get(k).push(r); }
  return m;
};

for (const dir of dirs) {
  const envPath = path.join(dir, "env.json");
  const closedPath = path.join(dir, "closed.json");
  const openPath = path.join(dir, "open.json");
  const env = fs.existsSync(envPath) ? JSON.parse(fs.readFileSync(envPath, "utf8")) : {};
  const closed = fs.existsSync(closedPath) ? JSON.parse(fs.readFileSync(closedPath, "utf8")) : [];
  const open = fs.existsSync(openPath) ? JSON.parse(fs.readFileSync(openPath, "utf8")) : [];

  console.log(`\n### Run \`${env.label || path.basename(dir)}\`\n`);
  console.log(`commit ${env.commit}, ${env.node}, ${env.cpuModel} (${env.logicalCpus} logical CPUs), build variant ${env.buildVariant}; server pinned to CPUs ${env.serverCpus}, mock upstream ${env.mockCpus} (delay ${env.mock?.delayMs}ms), generator ${env.genCpus}; server log level ${env.serverLogLevel}; ${env.config?.repeats} repeats x ${(env.config?.durationMs || 0) / 1000}s measured after ${(env.config?.warmupMs || 0) / 1000}s warm-up.\n`);

  const byScenario = group(closed, (r) => r.scenario);
  for (const [scenario, rows] of byScenario) {
    console.log(`#### ${scenario} - closed loop\n`);
    console.log("| c | req/s (mean ±95% CI) | err % | p50 ms | p90 ms | p95 ms | p99 ms | p99.9 ms | max ms | n/run | srv CPU cores | srv RSS peak MB | gen CPU |");
    console.log("|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|");
    const cells = [...group(rows, (r) => r.concurrency)].sort((a, b) => a[0] - b[0]);
    for (const [c, runs] of cells) {
      const l = (k) => runs.map((r) => r.latencyMs[k]).filter((v) => v !== undefined && v !== null);
      const p999Ok = runs.every((r) => r.latencyMs.p999Supported);
      console.log(`| ${c} | ${fmt(mean(runs.map((r) => r.throughput)), 0)} ±${fmt(ci95(runs.map((r) => r.throughput)), 0)} | ${fmt(mean(runs.map((r) => r.errorRate * 100)), 3)} | ${pm(l("p50"))} | ${pm(l("p90"))} | ${pm(l("p95"))} | ${pm(l("p99"))} | ${pm(l("p999"))}${p999Ok ? "" : "*"} | ${fmt(Math.max(...l("max")), 1)} | ${Math.round(mean(runs.map((r) => r.latencyMs.n)))} | ${fmt(mean(runs.map((r) => r.server.cpuCores)))} | ${fmt(Math.max(...runs.map((r) => r.server.rssMbPeak)), 0)} | ${fmt(mean(runs.map((r) => r.generatorCpuCores)))} |`);
    }
    console.log("\n`*` p99.9 rests on fewer than 10,000 samples per run (the top 0.1% is a handful of requests); treat it as indicative. `max` is the largest value over all repeats.\n");

    const pooledPath = path.join(dir, "pooled.json");
    const pooled = fs.existsSync(pooledPath) ? JSON.parse(fs.readFileSync(pooledPath, "utf8")).filter((x) => x.scenario === scenario) : [];
    if (pooled.length) {
      console.log("Exact percentiles over the pooled samples of all repeats of each cell (nearest rank on the full sorted set):\n");
      console.log("| c | pooled n | mean ms | stddev ms | p50 | p90 | p95 | p99 | p99.9 | max | cold-connect burst: all c sockets answered after (ms) | slowest first response (ms) |");
      console.log("|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|");
      for (const [c, runs] of cells) {
        const pl = pooled.find((x) => x.concurrency === c);
        if (!pl) continue;
        console.log(`| ${c} | ${pl.n} | ${fmt(pl.mean)} | ${fmt(pl.stddev)} | ${fmt(pl.p50)} | ${fmt(pl.p90)} | ${fmt(pl.p95)} | ${fmt(pl.p99)} | ${fmt(pl.p999)}${pl.p999Supported ? "" : "*"} | ${fmt(pl.max, 1)} | ${fmt(mean(runs.map((r) => r.connectBurst?.totalMs ?? NaN)), 0)} | ${fmt(Math.max(...runs.map((r) => r.connectBurst?.maxFirstResponseMs ?? NaN)), 0)} |`);
      }
      console.log("\n`*` pooled n < 10,000: p99.9 rests on fewer than 10 samples.\n");
    }
    const invalid = rows.filter((r) => r.valid === false);
    if (invalid.length) console.log(`**${invalid.length} run(s) flagged invalid (error rate > 0.1%)**: ${invalid.map((r) => `c=${r.concurrency} run ${r.run}`).join(", ")}\n`);

    // Little's law and knee analysis.
    const best = Math.max(...cells.map(([, runs]) => mean(runs.map((r) => r.throughput))));
    const unloaded = cells.find(([c]) => c === 1);
    const w1 = unloaded ? mean(unloaded[1].map((r) => r.latencyMs.mean)) / 1000 : null;
    const knee = cells.find(([, runs]) => mean(runs.map((r) => r.throughput)) >= 0.95 * best);
    console.log("| c | lambda req/s | W mean s | L = lambda*W | L sampled in flight | L error % | c (closed-loop population) |");
    console.log("|--:|--:|--:|--:|--:|--:|--:|");
    for (const [c, runs] of cells) {
      const lam = mean(runs.map((r) => r.little.lambdaPerSec));
      const w = mean(runs.map((r) => r.little.meanLatencySec));
      const pred = lam * w;
      const obs = mean(runs.map((r) => r.little.observedL));
      console.log(`| ${c} | ${fmt(lam, 1)} | ${fmt(w, 5)} | ${fmt(pred, 2)} | ${fmt(obs, 2)} | ${fmt(obs ? ((pred - obs) / obs) * 100 : 0, 2)} | ${c} |`);
    }
    console.log("");
    console.log(`- peak throughput lambda_max = **${fmt(best, 0)} req/s** (mean over repeats)`);
    if (w1) console.log(`- unloaded mean latency W(c=1) = ${fmt(w1 * 1000, 3)} ms, so the saturation population N* = lambda_max x W(1) = **${fmt(best * w1, 1)}** concurrent requests`);
    if (knee) console.log(`- smallest tested concurrency reaching 95% of lambda_max: **c = ${knee[0]}**`);
    const zeroErr = cells.filter(([, runs]) => runs.every((r) => r.errorRate === 0)).map(([c]) => c);
    console.log(`- highest tested concurrency with zero errors in every repeat: **c = ${zeroErr.length ? Math.max(...zeroErr) : "none"}**`);
    const coeffs = cells.map(([c, runs]) => ({ c, cv: (sd(runs.map((r) => r.throughput)) / mean(runs.map((r) => r.throughput))) * 100 }));
    console.log(`- run-to-run variance of throughput (coefficient of variation): ${coeffs.map((x) => `c=${x.c}: ${fmt(x.cv, 1)}%`).join(", ")}\n`);
  }

  if (open.length) {
    console.log("#### open loop (constant arrival rate, latency measured from the intended send time)\n");
    console.log("| scenario | target req/s | achieved req/s | err % | dropped | p50 ms | p90 ms | p95 ms | p99 ms | p99.9 ms | max ms | n/run | generator send lag p99 ms | srv CPU cores |");
    console.log("|--|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|");
    for (const [key, runs] of [...group(open, (r) => `${r.scenario}|${r.targetRate}`)].sort((a, b) => Number(a[0].split("|")[1]) - Number(b[0].split("|")[1]))) {
      const [scenario, rate] = key.split("|");
      const l = (k) => runs.map((r) => r.latencyMs[k]);
      const ok = runs.every((r) => r.latencyMs.p999Supported);
      console.log(`| ${scenario} | ${rate} | ${fmt(mean(runs.map((r) => r.throughput)), 1)} ±${fmt(ci95(runs.map((r) => r.throughput)), 1)} | ${fmt(mean(runs.map((r) => r.errorRate * 100)), 3)} | ${runs.reduce((a, r) => a + r.dropped, 0)} | ${pm(l("p50"))} | ${pm(l("p90"))} | ${pm(l("p95"))} | ${pm(l("p99"))} | ${pm(l("p999"))}${ok ? "" : "*"} | ${fmt(Math.max(...l("max")), 1)} | ${Math.round(mean(runs.map((r) => r.latencyMs.n)))} | ${fmt(Math.max(...runs.map((r) => r.generatorSendLagMs.p99)), 2)} | ${fmt(mean(runs.map((r) => r.server.cpuCores)))} |`);
    }
    console.log("");
    const pooledPath = path.join(dir, "pooled.json");
    const pooled = fs.existsSync(pooledPath) ? JSON.parse(fs.readFileSync(pooledPath, "utf8")).filter((x) => x.scenario?.startsWith("open:")) : [];
    if (pooled.length) {
      console.log("Open loop, exact percentiles over the pooled samples of all repeats (latency from the intended send time):\n");
      console.log("| target req/s | pooled n | p50 | p90 | p95 | p99 | p99.9 | max |");
      console.log("|--:|--:|--:|--:|--:|--:|--:|--:|");
      for (const pl of pooled.sort((a, b) => a.rate - b.rate)) console.log(`| ${pl.rate} | ${pl.n} | ${fmt(pl.p50)} | ${fmt(pl.p90)} | ${fmt(pl.p95)} | ${fmt(pl.p99)} | ${fmt(pl.p999)}${pl.p999Supported ? "" : "*"} | ${fmt(pl.max, 1)} |`);
      console.log("\n`*` pooled n < 10,000.\n");
    }
    const invalid = open.filter((r) => r.valid === false);
    if (invalid.length) console.log(`**${invalid.length} open-loop run(s) had error rate > 0.1%** (rate x run: ${invalid.map((r) => `${r.targetRate}x${r.run}`).join(", ")}); those are overload timeouts, kept in the table, not hidden.\n`);
  }
}
