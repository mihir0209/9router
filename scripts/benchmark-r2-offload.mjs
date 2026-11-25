#!/usr/bin/env node
/**
 * Benchmark D1 row size reduction (%) and R2 offload/hydration latency
 * on realistic 20 KB multi-turn `requestDetails` payloads.
 */

import { performance } from "node:perf_hooks";
import {
  offloadRequestDetailToR2,
  hydrateRequestDetailFromR2,
} from "../src/cloudflare/r2-details.js";

function buildRealistic20KbPayload(index) {
  const turns = [];
  const turnText = "Here is a detailed multi-turn architectural discussion with code snippets, stack traces, and tool outputs. ".repeat(14);
  for (let i = 0; i < 6; i++) {
    turns.push({ role: i % 2 === 0 ? "user" : "assistant", content: `[turn ${i}] ${turnText}` });
  }
  const responseText = "Comprehensive assistant response summarizing changes, edge cases, and verification steps. ".repeat(35);

  return {
    id: `bench-req-${index}-${Date.now()}`,
    provider: "anthropic",
    model: "claude-sonnet-4-5",
    connectionId: "conn-prod-1",
    timestamp: "2026-11-25T10:30:00.000Z",
    status: "200",
    latency: { ttftMs: 142, totalMs: 1180 },
    tokens: { input: 4250, output: 910, cacheRead: 1200 },
    request: {
      method: "POST",
      url: "/v1/messages",
      headers: { "content-type": "application/json", "x-stainless-os": "Linux" },
      messages: turns,
    },
    providerRequest: {
      model: "claude-sonnet-4-5",
      max_tokens: 4096,
      messages: turns.slice(-4),
    },
    providerResponse: {
      id: `msg_${index}`,
      type: "message",
      role: "assistant",
      content: [{ type: "text", text: responseText }],
      usage: { input_tokens: 4250, output_tokens: 910 },
    },
    response: {
      id: `chatcmpl_${index}`,
      choices: [{ index: 0, message: { role: "assistant", content: responseText }, finish_reason: "stop" }],
    },
  };
}

function createInMemoryR2Mock() {
  const map = new Map();
  return {
    async put(key, value) {
      map.set(key, typeof value === "string" ? value : JSON.stringify(value));
    },
    async get(key) {
      const raw = map.get(key);
      if (raw === undefined) return null;
      return {
        async text() { return raw; },
        async json() { return JSON.parse(raw); },
      };
    },
  };
}

function percentile(sorted, p) {
  if (sorted.length === 0) return 0;
  const idx = Math.min(sorted.length - 1, Math.floor((p / 100) * sorted.length));
  return sorted[idx];
}

async function main() {
  const iterations = Number(process.env.BENCH_ITERATIONS || 100);
  const bucket = createInMemoryR2Mock();

  let totalOriginalBytes = 0;
  let totalCompactBytes = 0;
  const putLatenciesMs = [];
  const getLatenciesMs = [];
  const compactRecords = [];

  for (let i = 0; i < iterations; i++) {
    const payload = buildRealistic20KbPayload(i);
    const t0 = performance.now();
    const res = await offloadRequestDetailToR2(payload, { bucket });
    const t1 = performance.now();

    putLatenciesMs.push(t1 - t0);
    totalOriginalBytes += res.originalBytes;
    totalCompactBytes += res.compactBytes;
    compactRecords.push(res.record);
  }

  for (const compact of compactRecords) {
    const t0 = performance.now();
    await hydrateRequestDetailFromR2(compact, { bucket });
    const t1 = performance.now();
    getLatenciesMs.push(t1 - t0);
  }

  putLatenciesMs.sort((a, b) => a - b);
  getLatenciesMs.sort((a, b) => a - b);

  const avgOriginalBytes = Math.round(totalOriginalBytes / iterations);
  const avgCompactBytes = Math.round(totalCompactBytes / iterations);
  const reductionPct = ((1 - totalCompactBytes / totalOriginalBytes) * 100).toFixed(2);

  const summary = {
    iterations,
    avgOriginalBytes,
    avgCompactBytes,
    d1RowSizeReductionPct: Number(reductionPct),
    offloadLatencyMs: {
      avg: Number((putLatenciesMs.reduce((a, b) => a + b, 0) / iterations).toFixed(4)),
      p50: Number(percentile(putLatenciesMs, 50).toFixed(4)),
      p95: Number(percentile(putLatenciesMs, 95).toFixed(4)),
    },
    hydrateLatencyMs: {
      avg: Number((getLatenciesMs.reduce((a, b) => a + b, 0) / iterations).toFixed(4)),
      p50: Number(percentile(getLatenciesMs, 50).toFixed(4)),
      p95: Number(percentile(getLatenciesMs, 95).toFixed(4)),
    },
  };

  console.log(JSON.stringify(summary, null, 2));
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
