#!/usr/bin/env node
// Projected Cloudflare cost per 1,000,000 proxied requests, from the exact per-request operation
// counts measured by tests/unit/cloudflare-d1-ops.test.js (docs/benchmarks/d1-ops-*.json).
//
//   node scripts/bench/d1-cost.mjs [before.json] [after.json] > table.md
//
// List prices are Cloudflare's published Workers Paid rates, fetched from
//   https://developers.cloudflare.com/d1/platform/pricing/
//   https://developers.cloudflare.com/durable-objects/platform/pricing/
//   https://developers.cloudflare.com/workers/platform/pricing/
// on 2026-10-28. They are MARGINAL rates (the price of usage beyond the monthly included
// allotment); the included allotments (25 B D1 rows read, 50 M rows written, 1 M DO requests ...)
// are discussed separately in the report. Change PRICE below if Cloudflare changes them.
import fs from "node:fs";

export const PRICE = {
  fetchedOn: "2026-10-28",
  d1RowsReadPerMillion: 0.001, // after 25 billion/month included
  d1RowsWrittenPerMillion: 1.0, // after 50 million/month included
  doRequestsPerMillion: 0.15, // after 1 million/month included; each RPC method call is one request
  doSqlRowsReadPerMillion: 0.001, // SQLite-backed DO storage: same rates as D1
  doSqlRowsWrittenPerMillion: 1.0,
  doDurationPerMillionGbSeconds: 12.5, // 128 MB billed regardless of use
  doBilledMemoryGb: 0.125,
  workersRequestsPerMillion: 0.3, // after 10 million/month included
  workersCpuPerMillionMs: 0.02, // after 30 million CPU-ms/month included
  // Monthly included allotments on Workers Paid (each meter is billed only beyond these).
  included: { d1RowsRead: 25e9, d1RowsWritten: 50e6, doRequests: 1e6, doRowsRead: 25e9, doRowsWritten: 50e6, doGbSeconds: 400_000, workersRequests: 10e6 },
};
// Assumption (NOT measured): wall-clock time the coordinator is active per RPC.
export const ASSUMED_DO_MS_PER_RPC = 1;

const [beforePath = "docs/benchmarks/d1-ops-before.json", afterPath = "docs/benchmarks/d1-ops-after.json"] = process.argv.slice(2);
const read = (p) => JSON.parse(fs.readFileSync(p, "utf8"));

export function costPerMillion(a) {
  const M = 1_000_000;
  const d1Read = (a.d1RowsRead * M / 1e6) * PRICE.d1RowsReadPerMillion;
  const d1Write = (a.d1RowsWritten * M / 1e6) * PRICE.d1RowsWrittenPerMillion;
  const doReq = (a.doRpcs * M / 1e6) * PRICE.doRequestsPerMillion;
  const doRead = (a.doRowsRead * M / 1e6) * PRICE.doSqlRowsReadPerMillion;
  const doWrite = (a.doRowsWritten * M / 1e6) * PRICE.doSqlRowsWrittenPerMillion;
  const gbSeconds = (a.doRpcs * M * (ASSUMED_DO_MS_PER_RPC / 1000)) * PRICE.doBilledMemoryGb;
  const doDur = (gbSeconds / 1e6) * PRICE.doDurationPerMillionGbSeconds;
  return { d1Read, d1Write, doReq, doRead, doWrite, doDur, total: d1Read + d1Write + doReq + doRead + doWrite + doDur };
}

/** Monthly bill (USD, excluding the $5/month plan minimum and Workers CPU) for N requests/month with the included allotments applied. */
export function monthlyBill(a, requests) {
  const I = PRICE.included;
  const over = (used, inc, rate) => Math.max(0, used - inc) / 1e6 * rate;
  const gbSeconds = a.doRpcs * requests * (ASSUMED_DO_MS_PER_RPC / 1000) * PRICE.doBilledMemoryGb;
  return (
    over(a.d1RowsRead * requests, I.d1RowsRead, PRICE.d1RowsReadPerMillion) +
    over(a.d1RowsWritten * requests, I.d1RowsWritten, PRICE.d1RowsWrittenPerMillion) +
    over(a.doRpcs * requests, I.doRequests, PRICE.doRequestsPerMillion) +
    over(a.doRowsRead * requests, I.doRowsRead, PRICE.doSqlRowsReadPerMillion) +
    over(a.doRowsWritten * requests, I.doRowsWritten, PRICE.doSqlRowsWrittenPerMillion) +
    over(gbSeconds, I.doGbSeconds, PRICE.doDurationPerMillionGbSeconds)
  );
}

const usd = (x) => `$${x < 0.01 && x > 0 ? x.toFixed(4) : x.toFixed(2)}`;

if (import.meta.url === `file://${process.argv[1]}`) {
  const before = read(beforePath);
  const after = read(afterPath);
  const find = (r, label) => r.scenarios.find((s) => s.scenario === label)?.amortizedPerRequest;
  const labels = [
    "warm: chat non-stream x20",
    "warm: chat stream x20",
    "warm: chat pinned key (force pins + ACL) x20",
    "warm: chat rate-limited key x20",
    "warm: chat max-concurrent key x20",
    "warm: chat budget key x20",
    "warm: invalid API key x20",
    "warm: GET /v1/models x20",
    "cold: chat non-stream",
    "cold: GET /v1/models",
    "steady 1h @ 0.1 req/s (360 req)",
    "steady 1h @ 1 req/min (60 req)",
  ];
  console.log(`| Scenario | Before: D1 rd / wr · DO rpc / rd / wr | Before $/1M req | After: D1 rd / wr · DO rpc / rd / wr | After $/1M req | Saving |`);
  console.log("|---|---|--:|---|--:|--:|");
  const n = (v) => (Number.isInteger(v) ? String(v) : v.toFixed(3));
  const ops = (a) => `${n(a.d1RowsRead)} / ${n(a.d1RowsWritten)} · ${n(a.doRpcs)} / ${n(a.doRowsRead)} / ${n(a.doRowsWritten)}`;
  const rows = {};
  for (const label of labels) {
    const b = find(before, label);
    const a = find(after, label);
    if (!b || !a) continue;
    const cb = costPerMillion(b);
    const ca = costPerMillion(a);
    rows[label] = { cb, ca };
    const saving = cb.total > 0 ? `${(((cb.total - ca.total) / cb.total) * 100).toFixed(1)}%` : "n/a";
    console.log(`| ${label} | ${ops(b)} | ${usd(cb.total)} | ${ops(a)} | ${usd(ca.total)} | ${saving} |`);
  }
  // Mix of cold (new isolate) and warm requests.
  console.log("\nBlended chat non-stream cost per 1M requests when a fraction f of requests lands on a cold isolate (cost = f x cold + (1 - f) x warm):\n");
  console.log("| f cold | Before | After | Saving |");
  console.log("|--:|--:|--:|--:|");
  const cw = { b: costPerMillion(find(before, "warm: chat non-stream x20")), a: costPerMillion(find(after, "warm: chat non-stream x20")) };
  const cc = { b: costPerMillion(find(before, "cold: chat non-stream")), a: costPerMillion(find(after, "cold: chat non-stream")) };
  for (const f of [0, 0.001, 0.01, 0.05, 0.1]) {
    const b = f * cc.b.total + (1 - f) * cw.b.total;
    const a = f * cc.a.total + (1 - f) * cw.a.total;
    console.log(`| ${(f * 100).toFixed(1)}% | ${usd(b)} | ${usd(a)} | ${(((b - a) / b) * 100).toFixed(1)}% |`);
  }
  console.log("\nMonthly storage-operation bill for warm chat non-stream with the monthly included allotments applied (excludes the $5/month plan minimum, Workers request and CPU charges, which are identical before and after):\n");
  console.log("| Requests / month | Before | After |");
  console.log("|--:|--:|--:|");
  for (const req of [1e6, 10e6, 100e6, 1e9, 10e9]) {
    console.log(`| ${req.toLocaleString("en-US")} | ${usd(monthlyBill(find(before, "warm: chat non-stream x20"), req))} | ${usd(monthlyBill(find(after, "warm: chat non-stream x20"), req))} |`);
  }
  console.log("\nComponent breakdown, warm chat non-stream, per 1M requests:\n");
  console.log("| Component | Before | After |");
  console.log("|---|--:|--:|");
  for (const [k, name] of [["d1Read", "D1 rows read"], ["d1Write", "D1 rows written"], ["doReq", "DO requests (RPCs)"], ["doRead", "DO SQLite rows read"], ["doWrite", "DO SQLite rows written"], ["doDur", "DO duration (assumed 1 ms/RPC)"], ["total", "Total (excl. Workers requests/CPU)"]]) {
    console.log(`| ${name} | ${usd(cw.b[k])} | ${usd(cw.a[k])} |`);
  }
}
