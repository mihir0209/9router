#!/usr/bin/env node
// Markdown tables for the multi-process scaling runs written by scaling.mjs.
//   node scripts/bench/scaling-report.mjs docs/benchmarks/runs/scaling
// Reads <dir>/n1, n2, n4, n8 (closed.json, open.json, pooled.json, env.json).
import fs from "node:fs";
import path from "node:path";

const dir = process.argv[2];
const Ns = [1, 2, 4, 8].filter((n) => fs.existsSync(path.join(dir, `n${n}`, "closed.json")));
const read = (n, f) => JSON.parse(fs.readFileSync(path.join(dir, `n${n}`, f), "utf8"));
const mean = (xs) => xs.reduce((a, b) => a + b, 0) / xs.length;
const sd = (xs) => (xs.length > 1 ? Math.sqrt(xs.reduce((a, b) => a + (b - mean(xs)) ** 2, 0) / (xs.length - 1)) : 0);
const T95 = [NaN, 12.706, 4.303, 3.182, 2.776, 2.571];
const ci95 = (xs) => (xs.length > 1 ? (T95[xs.length - 1] * sd(xs)) / Math.sqrt(xs.length) : 0);
const f = (x, d = 1) => (x === null || x === undefined || Number.isNaN(x) ? "n/a" : x.toFixed(d));

const data = {};
for (const n of Ns) {
  const closed = read(n, "closed.json");
  const open = read(n, "open.json");
  const pooled = read(n, "pooled.json");
  const env = read(n, "env.json");
  const cells = {};
  for (const r of closed) (cells[r.concurrency] ||= []).push(r);
  const ocells = {};
  for (const r of open) (ocells[r.targetRate] ||= []).push(r);
  data[n] = { closed, open, pooled, env, cells, ocells };
}

console.log("#### Closed loop: throughput by server count\n");
console.log("Total connections c are split evenly over 4 generator processes. req/s is mean ± CI95 of 3 repeats. Efficiency = throughput(n) / (n × throughput(1)) at the same c.\n");
console.log("| N | c | req/s (mean ±CI95) | CV % | efficiency vs N=1 | errors | server CPU (cores, all processes) | CPU ms / request | balancer CPU | p50 ms | p99 ms | p99.9 ms | max ms |");
console.log("|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|");
for (const n of Ns) {
  for (const c of Object.keys(data[n].cells).map(Number).sort((a, b) => a - b)) {
    const runs = data[n].cells[c];
    const t = runs.map((r) => r.throughput);
    const base = data[1]?.cells[c] ? mean(data[1].cells[c].map((r) => r.throughput)) : null;
    const p = data[n].pooled.find((x) => x.key === String(c));
    const cpu = mean(runs.map((r) => r.serverCpuTotal));
    console.log(`| ${n} | ${c} | ${f(mean(t), 0)} ±${f(ci95(t), 0)} | ${f((sd(t) / mean(t)) * 100)} | ${base ? f(mean(t) / (n * base), 2) : "n/a"} | ${runs.reduce((a, r) => a + r.failed, 0)} | ${f(cpu, 2)} | ${f((cpu / mean(t)) * 1000, 2)} | ${f(mean(runs.map((r) => r.balancer.cpuCores)), 2)} | ${f(p?.p50, 1)} | ${f(p?.p99, 0)} | ${f(p?.p999, 0)} | ${f(p?.max, 0)} |`);
  }
}

console.log("\n#### Peak closed-loop throughput and scaling efficiency\n");
console.log("Peak = best mean over the tested c. The c giving the peak is shown.\n");
console.log("| N | peak req/s (mean ±CI95) | at c | efficiency | speed-up vs N=1 | server CPU at peak (cores) | CPU ms / request | per-process req/s |");
console.log("|--:|--:|--:|--:|--:|--:|--:|--:|");
const peak = {};
for (const n of Ns) {
  let best = null;
  for (const [c, runs] of Object.entries(data[n].cells)) {
    const m = mean(runs.map((r) => r.throughput));
    if (!best || m > best.m) best = { c: Number(c), m, runs };
  }
  peak[n] = best;
}
for (const n of Ns) {
  const b = peak[n];
  const t = b.runs.map((r) => r.throughput);
  const cpu = mean(b.runs.map((r) => r.serverCpuTotal));
  console.log(`| ${n} | ${f(b.m, 0)} ±${f(ci95(t), 0)} | ${b.c} | ${f(b.m / (n * peak[1].m), 2)} | ${f(b.m / peak[1].m, 2)}x | ${f(cpu, 2)} | ${f((cpu / b.m) * 1000, 2)} | ${f(b.m / n, 0)} |`);
}

console.log("\n#### Open loop (constant arrival rate, latency measured from the intended send time)\n");
console.log("Rates are 250 x N and 400 x N req/s plus, for N >= 2, nothing else; pooled percentiles are exact over the union of the 3 repeats (nearest rank). `dropped` = arrivals the generators could not issue because 600 requests per generator were already in flight (the system was beyond capacity). Runs with dropped arrivals or a queue that grows for the whole window are overload, not service latency.\n");
console.log("| N | target req/s | achieved req/s | errors | dropped/run | server CPU (cores) | per-process CPU | n pooled | p50 | p90 | p95 | p99 | p99.9 | max |");
console.log("|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|");
for (const n of Ns) {
  for (const rate of Object.keys(data[n].ocells).map(Number).sort((a, b) => a - b)) {
    const runs = data[n].ocells[rate];
    const p = data[n].pooled.find((x) => x.key === `open:${rate}`);
    const perProc = mean(runs.map((r) => r.serverCpuTotal)) / n;
    console.log(`| ${n} | ${rate} | ${f(mean(runs.map((r) => r.throughput)), 0)} | ${runs.reduce((a, r) => a + r.failed, 0)} | ${f(mean(runs.map((r) => r.dropped)), 0)} | ${f(mean(runs.map((r) => r.serverCpuTotal)), 2)} | ${f(perProc, 2)} | ${p?.n} | ${f(p?.p50, 2)} | ${f(p?.p90, 2)} | ${f(p?.p95, 2)} | ${f(p?.p99, 1)} | ${f(p?.p999, 1)} | ${f(p?.max, 1)} |`);
  }
}

console.log("\n#### Per-process balance (closed loop, c = 256)\n");
console.log("| N | CPU cores per process (mean of 3 runs) | balancer connections per backend |");
console.log("|--:|--|--|");
for (const n of Ns) {
  const runs = data[n].cells[256] || [];
  if (!runs.length) continue;
  const per = Array.from({ length: n }, (_, i) => f(mean(runs.map((r) => r.servers[i].cpuCores)), 2)).join(" / ");
  console.log(`| ${n} | ${per} | ${(data[n].env.balancerConnectionsPerBackend || []).join(" / ")} |`);
}
