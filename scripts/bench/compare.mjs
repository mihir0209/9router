#!/usr/bin/env node
// Before/after table for the same scenarios and concurrencies from two sets of run directories.
//   node scripts/bench/compare.mjs "<beforeDir>,<beforeDir>" "<afterDir>,<afterDir>" [c1,c2,...]
import fs from "node:fs";
import path from "node:path";

const [beforeArg, afterArg, cArg = "1,8,32,128"] = process.argv.slice(2);
const wantC = cArg.split(",").map(Number);
const load = (dirs) => {
  const cells = new Map();
  for (const dir of dirs.split(",")) {
    const closed = JSON.parse(fs.readFileSync(path.join(dir, "closed.json"), "utf8"));
    const pooled = JSON.parse(fs.readFileSync(path.join(dir, "pooled.json"), "utf8"));
    for (const r of closed) {
      const key = `${r.scenario}|${r.concurrency}`;
      if (!cells.has(key)) cells.set(key, { runs: [], pooled: null });
      cells.get(key).runs.push(r);
    }
    for (const p of pooled) if (cells.has(`${p.scenario}|${p.concurrency}`)) cells.get(`${p.scenario}|${p.concurrency}`).pooled = p;
  }
  return cells;
};
const mean = (xs) => xs.reduce((a, b) => a + b, 0) / xs.length;
const before = load(beforeArg);
const after = load(afterArg);
console.log("| Scenario | c | req/s before | req/s after | change | p50 ms before -> after | p99 ms before -> after | p99.9 ms before -> after |");
console.log("|--|--:|--:|--:|--:|--|--|--|");
for (const [key, a] of after) {
  const b = before.get(key);
  if (!b) continue;
  const [scenario, c] = key.split("|");
  if (!wantC.includes(Number(c))) continue;
  const tb = mean(b.runs.map((r) => r.throughput));
  const ta = mean(a.runs.map((r) => r.throughput));
  const f = (x) => x.toFixed(2);
  console.log(`| ${scenario} | ${c} | ${tb.toFixed(0)} | ${ta.toFixed(0)} | ${(((ta - tb) / tb) * 100).toFixed(1)}% | ${f(b.pooled.p50)} -> ${f(a.pooled.p50)} | ${f(b.pooled.p99)} -> ${f(a.pooled.p99)} | ${f(b.pooled.p999)} -> ${f(a.pooled.p999)} |`);
}
