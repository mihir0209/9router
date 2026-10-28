// Load generator used by the benchmark orchestrator (scripts/bench/run-bench.mjs).
//
// Two modes, both on undici with one socket per concurrent request:
//
//   closedLoop  N workers, each sends the next request as soon as the previous one
//               finished. Measures peak throughput at a given concurrency. Latency
//               in this mode is subject to coordinated omission (a stalled server
//               slows the arrival rate), so it is reported as "service latency",
//               not as what a constant-rate client population would see.
//
//   openLoop    Requests are *scheduled* at a constant rate. Latency is measured
//               from the intended send time, so queueing behind a slow server is
//               counted (coordinated-omission-free), and the actual send lag is
//               tracked separately to show the generator itself kept up.
//
// Percentiles are computed from the full sorted sample set (nearest-rank:
// value at index ceil(q*n)-1). No histogram, no interpolation, no sampling.
import { Pool } from "undici";
import { performance } from "node:perf_hooks";

export function percentile(sorted, q) {
  if (!sorted.length) return null;
  const rank = Math.ceil(q * sorted.length);
  return sorted[Math.min(sorted.length - 1, Math.max(0, rank - 1))];
}

export function summarize(latenciesMs) {
  const sorted = Float64Array.from(latenciesMs).sort();
  const n = sorted.length;
  if (!n) return { n: 0 };
  let sum = 0;
  for (let i = 0; i < n; i++) sum += sorted[i];
  const mean = sum / n;
  let sq = 0;
  for (let i = 0; i < n; i++) sq += (sorted[i] - mean) ** 2;
  return {
    n,
    min: sorted[0],
    mean,
    stddev: n > 1 ? Math.sqrt(sq / (n - 1)) : 0,
    p50: percentile(sorted, 0.5),
    p90: percentile(sorted, 0.9),
    p95: percentile(sorted, 0.95),
    p99: percentile(sorted, 0.99),
    p999: percentile(sorted, 0.999),
    max: sorted[n - 1],
    // A quantile q is only supported by the data when there are >= 1/(1-q) samples
    // beyond it; flag the ones that are not (p99.9 needs >= 1000 samples, ideally 10000).
    p999Supported: n >= 10_000,
    p99Supported: n >= 1_000,
  };
}

function makePool(origin, connections) {
  return new Pool(origin, {
    connections,
    pipelining: 1,
    keepAliveTimeout: 60_000,
    keepAliveMaxTimeout: 120_000,
    headersTimeout: 60_000,
    bodyTimeout: 60_000,
  });
}

async function fire(pool, spec) {
  try {
    const res = await pool.request({
      path: spec.path,
      method: spec.method || "GET",
      headers: spec.headers,
      body: spec.body,
    });
    // Drain the body: for SSE that means waiting for the whole stream.
    let bytes = 0;
    for await (const chunk of res.body) bytes += chunk.length;
    return { status: res.statusCode, bytes };
  } catch (error) {
    return { status: 0, bytes: 0, error: error.code || error.name || "error" };
  }
}

function isOk(result, spec) {
  const expect = spec.expectStatus ?? 200;
  return result.status === expect;
}

/** Sample the number of in-flight requests every `intervalMs` (independent check for Little's law). */
function startInflightSampler(getInflight, from, to, intervalMs = 5) {
  const samples = [];
  // Only samples inside the measurement window [from, to] count, so the warm-up ramp and the
  // post-window drain do not bias the time-average used in the Little's law cross-check.
  const timer = setInterval(() => {
    const now = performance.now();
    if (now >= from && now <= to) samples.push(getInflight());
  }, intervalMs);
  return () => {
    clearInterval(timer);
    return samples;
  };
}

export async function closedLoop({ origin, spec, concurrency, durationMs, warmupMs = 0 }) {
  const pool = makePool(origin, concurrency);
  const latencies = [];
  const errors = {};
  let inflight = 0;
  let okCount = 0;
  let failCount = 0;
  let bytes = 0;
  let t0, measureStart, end; // set after the connection barrier below
  let inflightSamples = null;
  let stopSampler = null;
  const perSecond = new Map();
  const slowest = []; // top-5 slowest completions with their start/end offsets (ms since t0), for tail diagnosis

  async function worker() {
    while (performance.now() < end) {
      const start = performance.now();
      inflight++;
      const result = await fire(pool, spec);
      inflight--;
      const done = performance.now();
      // Count by COMPLETION time: lambda = completions in the window / window length, and W is the
      // mean latency of exactly those completions. Requests that straddle the window edges then
      // cancel out, which keeps Little's law (L = lambda * W) free of an edge-effect bias at
      // latencies that are a sizeable fraction of the window.
      if (done < measureStart || done > end) continue;
      const ok = isOk(result, spec);
      if (ok) {
        okCount++;
        latencies.push(done - start);
        if (slowest.length < 5 || done - start > slowest[slowest.length - 1].ms) {
          slowest.push({ ms: done - start, startedAtMs: start - t0, endedAtMs: done - t0 });
          slowest.sort((a, b) => b.ms - a.ms);
          if (slowest.length > 5) slowest.pop();
        }
        bytes += result.bytes;
        const bucket = Math.floor((done - measureStart) / 1000);
        perSecond.set(bucket, (perSecond.get(bucket) || 0) + 1);
      } else {
        failCount++;
        const key = result.error || `http_${result.status}`;
        errors[key] = (errors[key] || 0) + 1;
      }
    }
  }

  // Connection barrier: open all `concurrency` sockets and complete one request on each BEFORE the
  // clock starts. A real keep-alive client population does not reconnect every request; measuring
  // the cold-connect burst separately keeps the steady-state tail (p99.9, max) from being dominated
  // by a handful of TCP-handshake stalls (observed: single 2-4 s SYN/accept stalls in the first
  // second of a 128+ connection burst). Its duration is reported as `connectBurst`.
  const burstStart = performance.now();
  const burstResults = await Promise.all(Array.from({ length: concurrency }, async () => {
    const s = performance.now();
    const r = await fire(pool, spec);
    return { ms: performance.now() - s, ok: isOk(r, spec) };
  }));
  const connectBurst = {
    connections: concurrency,
    totalMs: performance.now() - burstStart,
    maxFirstResponseMs: Math.max(...burstResults.map((b) => b.ms)),
    p50FirstResponseMs: burstResults.map((b) => b.ms).sort((a, b) => a - b)[Math.ceil(0.5 * burstResults.length) - 1],
    failed: burstResults.filter((b) => !b.ok).length,
  };
  t0 = performance.now();
  measureStart = t0 + warmupMs;
  end = measureStart + durationMs;
  const workers = Array.from({ length: concurrency }, () => worker());
  if (warmupMs > 0) await new Promise((r) => setTimeout(r, warmupMs));
  stopSampler = startInflightSampler(() => inflight, measureStart, end);
  await Promise.all(workers);
  inflightSamples = stopSampler();
  await pool.close();

  const seconds = durationMs / 1000;
  const lat = summarize(latencies);
  const total = okCount + failCount;
  const meanInflight = inflightSamples.length ? inflightSamples.reduce((a, b) => a + b, 0) / inflightSamples.length : 0;
  const lambda = okCount / seconds;
  return {
    mode: "closed",
    concurrency,
    connectBurst,
    samples: latencies, // stripped by run-bench before writing; used to pool samples across repeats
    durationMs,
    warmupMs,
    ok: okCount,
    failed: failCount,
    errorRate: total ? failCount / total : 0,
    errors,
    throughput: lambda,
    bytesPerSec: bytes / seconds,
    latencyMs: lat,
    // Little's law: L = lambda * W. W is the mean latency of completed requests,
    // L_observed is the time-average of sampled in-flight requests.
    little: {
      lambdaPerSec: lambda,
      meanLatencySec: lat.mean ? lat.mean / 1000 : 0,
      predictedL: lat.mean ? lambda * (lat.mean / 1000) : 0,
      observedL: meanInflight,
      inflightSamples: inflightSamples.length,
    },
    slowest,
    perSecond: [...perSecond.entries()].sort((a, b) => a[0] - b[0]).map(([, v]) => v),
  };
}

export async function openLoop({ origin, spec, rate, durationMs, warmupMs = 0, maxInflight = 20_000 }) {
  const pool = makePool(origin, Math.min(maxInflight, Math.max(64, Math.ceil(rate * 2))));
  const latencies = []; // from intended send time (CO-free)
  const serviceLatencies = []; // from actual send
  const sendLag = []; // actual - intended
  const errors = {};
  let inflight = 0;
  let okCount = 0;
  let failCount = 0;
  let dropped = 0;
  const interval = 1000 / rate;
  const t0 = performance.now();
  const measureStart = t0 + warmupMs;
  const end = measureStart + durationMs;
  const total = Math.floor((warmupMs + durationMs) / interval);
  const pending = [];
  let stopSampler = null;
  let index = 0;

  const issue = (i) => {
    const intended = t0 + i * interval;
    const actual = performance.now();
    if (inflight >= maxInflight) {
      if (intended >= measureStart) dropped++;
      return;
    }
    inflight++;
    const p = fire(pool, spec).then((result) => {
      inflight--;
      const done = performance.now();
      if (intended < measureStart || intended >= end) return;
      if (isOk(result, spec)) {
        okCount++;
        latencies.push(done - intended);
        serviceLatencies.push(done - actual);
        sendLag.push(actual - intended);
      } else {
        failCount++;
        const key = result.error || `http_${result.status}`;
        errors[key] = (errors[key] || 0) + 1;
      }
    });
    pending.push(p);
  };

  stopSampler = startInflightSampler(() => inflight, measureStart, end);
  await new Promise((resolve) => {
    const tick = () => {
      const now = performance.now();
      while (index < total && t0 + index * interval <= now) issue(index++);
      if (index >= total) return resolve();
      setTimeout(tick, 1);
    };
    tick();
  });
  await Promise.all(pending);
  const inflightSamples = stopSampler();
  await pool.close();

  const seconds = durationMs / 1000;
  const lat = summarize(latencies);
  const svc = summarize(serviceLatencies);
  const lag = summarize(sendLag);
  const totalReq = okCount + failCount;
  const meanInflight = inflightSamples.length ? inflightSamples.reduce((a, b) => a + b, 0) / inflightSamples.length : 0;
  return {
    mode: "open",
    samples: latencies,
    targetRate: rate,
    durationMs,
    warmupMs,
    ok: okCount,
    failed: failCount,
    dropped,
    errorRate: totalReq ? failCount / totalReq : 0,
    errors,
    throughput: okCount / seconds,
    latencyMs: lat, // coordinated-omission-free (from intended start)
    serviceLatencyMs: svc, // from actual send
    generatorSendLagMs: { p50: lag.p50, p99: lag.p99, max: lag.max },
    little: {
      lambdaPerSec: okCount / seconds,
      meanLatencySec: svc.mean ? svc.mean / 1000 : 0,
      predictedL: svc.mean ? (okCount / seconds) * (svc.mean / 1000) : 0,
      observedL: meanInflight,
      inflightSamples: inflightSamples.length,
    },
  };
}
