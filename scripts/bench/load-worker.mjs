#!/usr/bin/env node
// One load-generator process of the multi-process scaling benchmark. The orchestrator starts several of
// these (each pinned to its own CPU) because a single Node generator tops out near 3,000 req/s.
//   node load-worker.mjs '<json {mode, origin, spec, concurrency|rate, durationMs, warmupMs, maxInflight, out}>'
import fs from "node:fs";
import { closedLoop, openLoop } from "./load.mjs";

const job = JSON.parse(process.argv[2]);
const result = job.mode === "open"
  ? await openLoop({ origin: job.origin, spec: job.spec, rate: job.rate, durationMs: job.durationMs, warmupMs: job.warmupMs, maxInflight: job.maxInflight })
  : await closedLoop({ origin: job.origin, spec: job.spec, concurrency: job.concurrency, durationMs: job.durationMs, warmupMs: job.warmupMs });
fs.writeFileSync(job.out, JSON.stringify(result));
process.exit(0);
