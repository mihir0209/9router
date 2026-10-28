#!/usr/bin/env node
// Render docs/benchmarks/d1-ops-before.json and d1-ops-after.json (written by
// tests/unit/cloudflare-d1-ops.test.js via D1_OPS_REPORT) as a markdown table.
//   node scripts/bench/d1-ops-table.mjs [before.json] [after.json]
import fs from "node:fs";

const [beforePath = "docs/benchmarks/d1-ops-before.json", afterPath = "docs/benchmarks/d1-ops-after.json"] = process.argv.slice(2);
const read = (p) => JSON.parse(fs.readFileSync(p, "utf8"));
const before = read(beforePath);
const after = read(afterPath);
const find = (report, label) => report.scenarios.find((s) => s.scenario === label);
const n = (v) => (Number.isInteger(v) ? String(v) : v.toFixed(3));
const cell = (s) => {
  if (!s) return "n/a";
  const a = s.amortizedPerRequest;
  return `${n(a.d1RowsRead)} / ${n(a.d1RowsWritten)} / ${n(a.d1Queries)} · ${n(a.doRpcs)} / ${n(a.doRowsRead)} / ${n(a.doRowsWritten)}`;
};

const rows = after.scenarios
  .filter((s) => !/\[(standard|extreme)\]/.test(s.scenario))
  .map((s) => `| ${s.scenario} | ${cell(find(before, s.scenario))} | ${cell(s)} |`);

console.log("| Scenario (per request) | Before: D1 rows read / written / queries · DO rpcs / rows read / rows written | After (extreme) |");
console.log("|---|---|---|");
console.log(rows.join("\n"));
