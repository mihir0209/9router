#!/usr/bin/env node
// One-row-per-scenario headline table from run directories (closed loop, pooled exact percentiles).
//   node scripts/bench/summary.mjs docs/benchmarks/runs/node-main-1 docs/benchmarks/runs/node-main-2 ...
import fs from "node:fs";
import path from "node:path";

const mean = (xs) => xs.reduce((a, b) => a + b, 0) / xs.length;
const f = (x, d = 2) => x.toFixed(d);
const scenarios = new Map();
for (const dir of process.argv.slice(2)) {
  const closed = JSON.parse(fs.readFileSync(path.join(dir, "closed.json"), "utf8"));
  const pooled = JSON.parse(fs.readFileSync(path.join(dir, "pooled.json"), "utf8")).filter((p) => p.concurrency !== undefined);
  for (const r of closed) {
    if (!scenarios.has(r.scenario)) scenarios.set(r.scenario, { runs: [], pooled: [] });
    scenarios.get(r.scenario).runs.push(r);
  }
  for (const p of pooled) scenarios.get(p.scenario).pooled.push(p);
}
console.log("Unloaded (c = 1, pooled over all repeats; latency in ms):\n");
console.log("| Scenario | pooled n | req/s | p50 | p90 | p95 | p99 | p99.9 | max | error rate |");
console.log("|--|--:|--:|--:|--:|--:|--:|--:|--:|--:|");
for (const [name, s] of scenarios) {
  const p = s.pooled.find((x) => x.concurrency === 1);
  const runs = s.runs.filter((r) => r.concurrency === 1);
  console.log(`| ${name} | ${p.n} | ${f(mean(runs.map((r) => r.throughput)), 0)} | ${f(p.p50)} | ${f(p.p90)} | ${f(p.p95)} | ${f(p.p99)} | ${f(p.p999)} | ${f(p.max, 1)} | ${f(mean(runs.map((r) => r.errorRate)) * 100, 3)}% |`);
}
console.log("\nSaturation (closed loop; N* = peak req/s x unloaded mean latency, Little's law):\n");
console.log("| Scenario | peak req/s (mean of 3 runs, cell c) | CV of that cell | W(1) ms | N* | smallest c with >= 95% of peak | server CPU cores at peak | server RSS peak MB (c=1024 or max tested) | max c with 0 errors |");
console.log("|--|--:|--:|--:|--:|--:|--:|--:|--:|");
for (const [name, s] of scenarios) {
  const cs = [...new Set(s.runs.map((r) => r.concurrency))].sort((a, b) => a - b);
  const cell = (c) => s.runs.filter((r) => r.concurrency === c);
  const lam = (c) => mean(cell(c).map((r) => r.throughput));
  const sd = (xs) => { const m = mean(xs); return Math.sqrt(xs.reduce((a, b) => a + (b - m) ** 2, 0) / (xs.length - 1)); };
  const peakC = cs.reduce((best, c) => (lam(c) > lam(best) ? c : best), cs[0]);
  const w1 = mean(cell(1).map((r) => r.latencyMs.mean)) / 1000;
  const knee = cs.find((c) => lam(c) >= 0.95 * lam(peakC));
  const zero = cs.filter((c) => cell(c).every((r) => r.errorRate === 0));
  const maxC = cs[cs.length - 1];
  console.log(`| ${name} | ${f(lam(peakC), 0)} (c=${peakC}) | ${f((sd(cell(peakC).map((r) => r.throughput)) / lam(peakC)) * 100, 1)}% | ${f(w1 * 1000, 3)} | ${f(lam(peakC) * w1, 1)} | ${knee} | ${f(mean(cell(peakC).map((r) => r.server.cpuCores)))} | ${f(Math.max(...cell(maxC).map((r) => r.server.rssMbPeak)), 0)} (c=${maxC}) | ${Math.max(...zero)} |`);
}
