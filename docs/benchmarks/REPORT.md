# 9router load test and Cloudflare cost report

Generated 2026-10-28 on commit `2eed5ba6` (runs taken on the working tree that became that commit; the
`commit` field in each `env.json` is its parent `6029c755`, the pre-fix runs under
`runs/pre-fix/` are at exactly `6029c755`).

Contents: [1 Summary](#1-summary) · [2 Environment](#2-environment) · [3 Method and math](#3-method-and-math) ·
[4 Node build results](#4-node-build-results) · [5 What the profile found and the fix](#5-what-the-profile-found-and-the-fix) ·
[6 Cloudflare build: projected cost](#6-cloudflare-build-projected-cost-per-1m-requests) ·
[7 Reproduce](#7-reproduce) · [8 Caveats and what is not verified](#8-caveats-and-what-is-not-verified) ·
[Appendix A: all generated tables](#appendix-a-all-generated-tables)

## 1. Summary

Everything below was measured against the **normal Node build** (`npm run build`, variant `node`,
`node custom-server.js`) with a **local mock upstream** (`scripts/bench/mock-upstream.mjs`). No real provider or
credential was used: the server ran on a scrubbed copy of the database in a temp `DATA_DIR`, with
`HTTP(S)_PROXY` pointed at a dead port.

| Question | Answer (chat completions, non-stream, valid key, instant upstream) |
|---|---|
| Peak throughput of one server process | **617 req/s** (c=128, mean of 3 runs, CV 0.6%); stream 530; pinned/ACL key 549 (non-stream) / 503 (stream); `GET /api/health` 1,931; `GET /v1/models` 172; auth failure (401) 1,235 |
| Latency, unloaded (c=1, n=10,463) | p50 **2.19 ms**, p90 2.72, p95 2.94, p99 **6.11**, p99.9 **8.25**, max 25.4 |
| Latency, open loop 300 req/s (n=13,500) | p50 3.01 ms, p90 3.93, p95 5.83, p99 10.98, p99.9 52.0, max 57.8 |
| Latency, open loop 500 req/s (n=22,500) | p50 4.34 ms, p90 11.4, p95 22.4, p99 42.2, p99.9 210, max 270 |
| Saturation knee | open loop: p50 stays within 4.4 ms up to **500 req/s** (p99 42 ms, p99.9 210 ms), degrading at 550 (p50 10 ms, p99 72 ms), queue grows without bound at **600** (p50 130 ms, p99.9 2.9 s). Closed loop: 95% of peak is reached at c=32 |
| Error rate | **0 errors in 1,308,875 requests** inside the measured windows of the fixed-build runs (195 closed + 30 open runs); the only errors anywhere are overload client timeouts in the pre-fix open loop beyond capacity |
| Max sustainable concurrency | **No failures up to the largest tested concurrency** (c=1024 for every scenario except `models`, which was tested to c=128; all repeats). The server is CPU bound on one core: latency W = c / λ grows linearly with c, so concurrency above N\* only adds queueing. N\* = λ_max x W(1) = 617 x 2.30 ms = **1.4** in-flight requests with an instant upstream; **32.7** with a 50 ms upstream |
| Process CPU / RSS | 1.15 cores (one saturated event-loop thread + GC/libuv helpers) at peak; RSS 590 MB at c=1, 640 MB at c=32, 1.18 GB at c=1024 (about 0.57 MB per extra in-flight connection) |
| Router cost per request | CPU 1.86 ms for a chat request (1.15 cores / 617 req/s); 0.60 ms for health; 0.92 ms for a 401; 6.0 ms for `/v1/models` |
| Little's law cross-check | L = λ·W matches the independently sampled in-flight count in all 195 closed-loop runs with median error 0.08%, 89% of runs within 1%, worst 13.1% (one c=1024 run, see caveats) |
| Real bottleneck found and fixed | Three per-request CPU wastes (RegExp rebuilt per glob, `Intl.DateTimeFormat` per log line, `statSync` per catalog lookup) cost ~25% of server CPU. Fix `2eed5ba6`: back-to-back A/B **355 -> 519 req/s (+46%)** at c=32, p50 92 -> 60 ms, p99 132 -> 92 ms |
| Cloudflare D1/DO cost (list price, per 1M warm chat requests) | **$4.56 -> $0.16 (-96.5%)**, cold-isolate request $10.19 -> $0.55. With monthly included allotments: 100M req/month **$394.85 -> $0.60**, 1B req/month $4,450 -> $57 |

Findings that are not fixed and worth a follow-up:

* `GET /v1/models` costs 6 ms CPU per call (172 req/s per process) with the large custom-model working set of the
  copied database. It is the slowest endpoint by an order of magnitude and a good candidate for a response cache.
* The default per-request INFO log lines are cheap now (silencing them gave +2.6% on chat, within run-to-run noise),
  so `LOG_LEVEL` is no longer a meaningful tuning knob on this build.
* One Node process uses ~1.15 cores. Throughput scales with processes behind a reverse proxy (not measured here).

## 2. Environment

| Item | Value |
|---|---|
| CPU | AMD Ryzen 7 5800H with Radeon Graphics, 8 cores / 16 logical CPUs, ~4.4 GHz reported |
| Memory | 15,312 MB total |
| OS | Linux 7.0.0-38-generic x86_64 |
| Node | v24.22.0 (`node:sqlite` driver) |
| Build | `npm run build`, build variant `node`, run as `node custom-server.js --port 20127` (same command `npm start` runs) |
| Placement | server pinned with `taskset` to CPUs 0-3, mock upstream to CPUs 4-5; the load generator is **not** pinned (it used 0.05-0.20 cores) |
| Data | copy of the developer's database, scrubbed (43 provider connections deactivated and their secrets replaced, 36 proxy pools neutralised, 7 API keys replaced), 8 combos, 6,376 kv rows. Plus the mock upstream node/connection and two generated throwaway API keys |
| Shared machine | a desktop, other applications were running (load average at the start of the fixed-build runs 1.7 - 7.3). The effect is quantified in section 8 |
| Mock upstream | `scripts/bench/mock-upstream.mjs`: OpenAI-compatible, instant response (8 SSE chunks when streaming) except the `node-mock-delay-50ms` runs (50 ms) |

Scenarios (`scripts/bench/run-bench.mjs`):

| Name | Request | Key |
|---|---|---|
| `health` | `GET /api/health` | none |
| `models` | `GET /v1/models` | valid key (measured to c=128 only, see caveats) |
| `chat-ns-plain` | `POST /v1/chat/completions`, `stream:false`, model `mock/mock-model` | plain valid key |
| `chat-stream-plain` | same, `stream:true`, response drained to `[DONE]` | plain valid key |
| `chat-ns-acl` / `chat-stream-acl` | same | key with `forceProvider`/`forceModel` pins, `allowedModels`, `blockedModels`, `allowedEndpoints`, a rate limit and a concurrent-request limit set (so every request runs the pin, ACL, rate-limit and lease code); the body asks for `anything/else` and the pin rewrites it |
| `chat-401` | same as chat, invalid key, expects HTTP 401 | invalid |

Before any run a sanity request checks the response (status, `choices`, `[DONE]`, non-empty model list), so a
mis-configured scenario aborts instead of producing latency numbers for error responses.

## 3. Method and math

### Load generation (`scripts/bench/load.mjs`)

* **Closed loop** (max throughput at a fixed concurrency): `c` workers on `c` keep-alive sockets (undici `Pool`, one
  request in flight per socket), each sending the next request when the previous one finished.
  Concurrencies 1, 8, 32, 128, 512, 1024. Closed loop suffers *coordinated omission* (a slow server slows the arrival
  rate), so its latencies are service latencies at that population, not what a constant-rate population would see.
* **Open loop** (constant arrival rate, coordinated-omission free): request *i* is scheduled at `t0 + i/rate` and its
  latency is measured **from that intended send time**, so time spent queueing behind a slow server (including inside
  the generator's pool) is counted. The generator's own send lag is recorded (p99 1.0 - 1.8 ms at every rate). That lag is part of every open-loop latency
  (so open-loop p50 3.65 ms at 100 req/s sits above closed-loop c=1 p50 2.19 ms) and is small against the multi-millisecond
  and larger tails that matter, which is why the generator is not the limit.
* **Connection barrier**: before the clock starts, all `c` sockets are opened and one request completes on each. The
  cold-connect burst is measured and reported separately (column "cold-connect burst" in Appendix A). An earlier
  version without the barrier let single 2-4 s TCP handshake stalls at the start of 128+ connection runs dominate
  p99.9 and max; those first-version runs were discarded and the whole matrix re-run.
* **Warm-up discarded**: a fresh server process per scenario (no state carried over), a 4 s warm-up on the scenario's
  own path, and a per-step warm-up of `max(2 s, 3 x expected W)` that is not counted. Measured window
  `max(8 s, 4 x expected W)` (capped at 24 s). Requests are counted by **completion time inside the window**.
* **Repeats**: 3 repeats per cell (4 for the A/B), each with its own exact statistics; cells are then summarised as
  mean ± sample standard deviation and a 95% confidence interval.
* **Validity**: a run is flagged invalid if more than 0.1% of requests fail or none succeeded. Failed requests are never
  added to the latency samples; the error rate is reported for every cell. No closed-loop run was flagged. The only
  flagged runs are the six pre-fix open-loop runs at 380 and 420 req/s that were beyond capacity (0.16 - 0.65% client
  timeouts after 60 s), kept visible in `runs/pre-fix/node-open-knee`.

### Statistics

For one run with `n` successful samples sorted ascending `x_(1) <= ... <= x_(n)`:

* **Percentile q** (nearest rank, no interpolation, no histogram, no sampling): `p_q = x_(ceil(q·n))`.
  Computed from the full sorted `Float64Array` of every sample (`summarize()` in `load.mjs`).
  p99 needs n ≥ 1,000 and p99.9 n ≥ 10,000 to be backed by ≥ 10 samples beyond the quantile; cells where that does not hold
  are starred in Appendix A.
* **Mean** `x̄ = Σx/n`, **sample standard deviation** `s = sqrt(Σ(x - x̄)² / (n-1))`.
* **Pooled percentiles**: the samples of all repeats of a cell are concatenated and the exact percentile is taken again over
  the union (n = Σ n_run). The headline tables use the pooled values; the per-run tables show mean ± s of the per-run values.
* **Across repeats** (k = 3): `CI95 = t(0.975, k-1) · s / sqrt(k)` with t = 4.303 for k = 3 (table in `report.mjs`).
  Coefficient of variation `CV = s / mean`.
* **Throughput** `λ = completions in window / window length`.
* **Little's law** `L = λ·W`: `W` is the mean latency of the completed requests, `L_obs` is the time-average of an
  independent in-flight counter sampled every 5 ms inside the window. In a closed loop L must equal c, so agreement
  validates the instrumentation (and the window accounting); in the open loop it is a genuine check of steady state.
* **Saturation population** `N* = λ_max · W(1)`: with `W(1)` the unloaded mean latency, N\* is the number of requests that must be in
  flight to keep the bottleneck resource busy. Raising concurrency beyond it cannot raise throughput, only latency
  (`W = c/λ_max`). **Knee** = smallest tested c reaching 95% of λ_max (closed) or the rate where the open-loop queue stops
  being bounded.
* **Process CPU and RSS** are read from `/proc/<pid>/stat` and `statm` for the server process tree, per run.

## 4. Node build results

All tables are for the fixed build (`2eed5ba6`), default INFO logging, instant mock upstream, unless stated.
Full per-cell tables (all six concurrencies, per-run mean ± s, pooled exact percentiles, Little's law, CV) are in
[Appendix A](#appendix-a-all-generated-tables). Raw per-run JSON: `docs/benchmarks/runs/*/{closed,open,pooled,env}.json`.

### 4.1 Unloaded latency and per-scenario peak

Unloaded (c = 1, pooled over 3 repeats; ms):

| Scenario | pooled n | req/s | p50 | p90 | p95 | p99 | p99.9 | max | error rate |
|--|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| health | 35667 | 1486 | 0.59 | 0.81 | 0.99 | 1.74 | 6.72 | 27.4 | 0.000% |
| models | 3874 | 161 | 6.05 | 6.46 | 6.72 | 11.84 | 13.97* | 22.4 | 0.000% |
| chat-401 | 26277 | 1095 | 0.84 | 1.00 | 1.10 | 1.76 | 6.24 | 21.8 | 0.000% |
| chat-ns-plain | 10463 | 436 | 2.19 | 2.72 | 2.94 | 6.11 | 8.25 | 25.4 | 0.000% |
| chat-stream-plain | 9248 | 385 | 2.47 | 3.05 | 3.36 | 6.30 | 7.74* | 23.1 | 0.000% |
| chat-ns-acl | 10350 | 431 | 2.20 | 2.73 | 2.97 | 5.92 | 7.22 | 31.5 | 0.000% |
| chat-stream-acl | 8202 | 342 | 2.74 | 3.47 | 4.02 | 6.90 | 11.07* | 14.1 | 0.000% |

`*` pooled n < 10,000: the p99.9 rests on fewer than 10 samples.

Saturation (closed loop):

| Scenario | peak req/s (mean of 3, at c) | CV of that cell | W(1) ms | N\* | smallest c with >= 95% of peak | server CPU cores at peak | server RSS peak MB | max tested c with 0 errors |
|--|--:|--:|--:|--:|--:|--:|--:|--:|
| health | 1931 (c=512) | 0.7% | 0.678 | 1.3 | 32 | 1.16 | 938 (c=1024) | 1024 |
| models | 172 (c=128) | 1.0% | 6.195 | 1.1 | 8 | 1.03 | 510 (c=128) | 128 |
| chat-401 | 1235 (c=512) | 0.4% | 0.914 | 1.1 | 8 | 1.13 | 851 (c=1024) | 1024 |
| chat-ns-plain | 617 (c=128) | 0.6% | 2.304 | 1.4 | 32 | 1.15 | 1178 (c=1024) | 1024 |
| chat-stream-plain | 530 (c=1024) | 2.8% | 2.598 | 1.4 | 128 | 1.19 | 1264 (c=1024) | 1024 |
| chat-ns-acl | 549 (c=32) | 9.4% | 2.324 | 1.3 | 32 | 1.11 | 1282 (c=1024) | 1024 |
| chat-stream-acl | 503 (c=1024) | 4.3% | 2.931 | 1.5 | 512 | 1.18 | 1241 (c=1024) | 1024 |

Reading: every scenario is limited by a single saturated event-loop thread (~1.1 cores), so peak throughput
is 1/CPU-per-request and N\* is about 1. The stream and pin/ACL/rate-limit/lease variants are not separable from the plain
non-stream path: they were measured in different sessions (differences of 11 - 14% here, versus ~13% session drift on the same
code), and in the back-to-back A/B stream and non-stream gave 518 and 519 req/s at c=32. The ACL cell is also the noisiest
(CV 9.4%). The later 95%-of-peak knee in the stream rows comes from that noise, not from streaming needing more concurrency.

### 4.2 Concurrency ramp, chat non-stream (`chat-ns-plain`)

Per-run mean ± 95% CI over 3 repeats for throughput, pooled exact percentiles for latency (ms):

| c | req/s ± CI95 | p50 | p90 | p95 | p99 | p99.9 | max | pooled n | server CPU cores | RSS peak MB | error rate |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 1 | 436 ±96 | 2.19 | 2.72 | 2.94 | 6.11 | 8.25 | 25.4 | 10463 | 1.08 | 593 | 0 |
| 8 | 543 ±39 | 13.95 | 18.15 | 19.18 | 22.58 | 45.71 | 60.9 | 13038 | 1.12 | 596 | 0 |
| 32 | 588 ±22 | 55.20 | 65.21 | 67.45 | 84.02 | 113.27 | 117.2 | 14118 | 1.13 | 637 | 0 |
| 128 | 617 ±10 | 191.64 | 250.66 | 256.41 | 290.33 | 342.20 | 342.5 | 14805 | 1.15 | 661 | 0 |
| 512 | 604 ±41 | 793.66 | 1058.31 | 1077.64 | 1123.03 | 1145.50 | 1148.1 | 14486 | 1.21 | 872 | 0 |
| 1024 | 601 ±63 | 1685.46 | 1820.54 | 2076.47 | 2210.29 | 2293.46 | 2295.6 | 14429 | 1.21 | 1178 | 0 |

(Every cell has pooled n > 10,000, so p99.9 is backed by at least 10 samples. Throughput is flat from c=32 up: past the knee, more
concurrency is pure queueing, `p50 ≈ c / λ`.)

Little's law for the same rows (L = λ·W vs the sampled in-flight count):

| c | λ req/s | W mean s | L = λ·W | L sampled | error |
|--:|--:|--:|--:|--:|--:|
| 1 | 436.0 | 0.00230 | 1.00 | 1.00 | 0.43% |
| 8 | 543.3 | 0.01473 | 8.00 | 8.00 | 0.03% |
| 32 | 588.3 | 0.05444 | 32.03 | 32.00 | 0.09% |
| 128 | 616.9 | 0.20713 | 127.77 | 128.00 | -0.18% |
| 512 | 603.6 | 0.83800 | 505.80 | 512.00 | -1.21% |
| 1024 | 601.2 | 1.70043 | 1022.31 | 1024.00 | -0.16% |

### 4.3 Open loop (constant arrival rate, coordinated-omission free), chat non-stream

3 repeats per rate of 15 s after a 2 s warm-up; latency from the intended send time; pooled exact percentiles (ms).
Nothing was dropped and there were 0 errors. (The reported "achieved" rate equals the target by construction, because requests are counted by intended send time; saturation shows up in the latency columns, not in that rate.)

| target req/s | pooled n | p50 | p90 | p95 | p99 | p99.9 | max | server CPU cores | generator send lag p99 |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 100 | 4500 | 3.65 | 4.24 | 4.48 | 7.34 | 10.87* | 22.0 | 0.33 | 1.06 ms |
| 200 | 9000 | 3.31 | 4.02 | 4.38 | 7.34 | 25.42* | 40.6 | 0.59 | 1.06 ms |
| 250 | 11250 | 3.12 | 3.93 | 4.59 | 8.43 | 38.25 | 44.4 | 0.66 | 1.14 ms |
| 300 | 13500 | 3.01 | 3.93 | 5.83 | 10.98 | 52.00 | 57.8 | 0.73 | 1.47 ms |
| 350 | 15750 | 2.89 | 4.08 | 7.12 | 23.42 | 74.88 | 98.7 | 0.81 | 1.44 ms |
| 400 | 18000 | 2.98 | 8.42 | 20.77 | 84.40 | 851.42 | 920.7 | 0.92 | 1.49 ms |
| 450 | 20250 | 3.09 | 9.99 | 19.09 | 61.83 | 215.62 | 382.3 | 1.03 | 1.67 ms |
| 500 | 22500 | 4.34 | 11.40 | 22.43 | 42.24 | 210.37 | 269.6 | 1.04 | 1.83 ms |
| 550 | 24750 | 10.09 | 40.29 | 49.79 | 71.55 | 407.30 | 502.0 | 1.09 | 1.61 ms |
| 600 | 27000 | 130.25 | 180.80 | 217.41 | 314.20 | 2948.18 | 3758.1 | 1.13 | 1.44 ms |

`*` pooled n < 10,000. The 100-400 rows come from `runs/node-main-3`, the 450-600 rows from `runs/node-open-knee`
(separate server processes, so the ordering at 400 vs 450 is within session noise: one of the three 400 req/s repeats
had a 920 ms excursion).

Knee: tail latency (p99/p99.9) starts to climb above ~350 req/s as utilisation passes ~0.55 of one core's capacity,
the p50 stays at or below 4.4 ms until 500 req/s, and at 600 req/s (utilisation ≥ 1) the queue grows for the whole 15 s window
(p50 130 ms and rising). The practical sustainable rate for a p99 under ~100 ms is **about 500 req/s** per process. The
closed-loop peak of 617 req/s is the absolute ceiling at which the CPU is 100% busy.

Open-loop Little check: L = λ·W against the sampled in-flight count agrees within ~2% at 250 - 600 req/s (for example
500 req/s: 3.25 vs 3.24; 600 req/s: 86.5 vs 87.2). At 200 req/s it differs by 20% (0.58 vs 0.71 in-flight requests in all three repeats): the 5 ms arrival interval equals the 5 ms sampler
period, so phase locking between the two is the likely cause (not investigated; the sampler is not jittered).

### 4.4 Upstream with 50 ms latency (`chat-ns-plain`, mock delay 50 ms)

| c | req/s ± CI95 | p50 | p99 | p99.9 | server CPU cores | RSS peak MB |
|--:|--:|--:|--:|--:|--:|--:|
| 1 | 19 ±0 | 53.75 | 57.15 | 57.83* | 0.09 | 391 |
| 32 | 494 ±24 | 64.01 | 87.81 | 104.00* | 1.01 | 609 |
| 128 | 607 ±9 | 208.14 | 268.89 | 279.43* | 1.15 | 668 |
| 1024 | 604 ±56 | 1802.13 | 1933.48 | 1936.67* | 1.20 | 1286 |

(Per-run means across 3 repeats; the full 9-point ramp is in Appendix A.) The router adds ~3.7 ms over the 50 ms upstream at
c=1 and is idle (0.09 cores) because the upstream is the bottleneck. With a slower upstream the saturation population
rises to **N\* = 607 req/s x 53.94 ms = 32.7 concurrent requests**: that is the concurrency at which one process becomes
CPU bound (the 95% knee is c=64 in the sampled ramp), and the peak throughput is unchanged (607 vs 617 req/s) because the
cost is router CPU, not waiting.

### 4.5 Cost of the default request logging

Same fixed build with `LOG_LEVEL=ERROR`: chat non-stream 633 ±22 req/s at c=128 versus 617 ±10 with INFO
(+2.6%, inside the run-to-run noise); health 1,933 vs 1,931. Before the `Intl.DateTimeFormat` fix, silencing logs
was worth +13 to +16% (pre-fix `LOG_LEVEL=ERROR` run vs pre-fix main run, c=8..128), so that cost has gone.

## 5. What the profile found and the fix

A V8 CPU profile of the server under chat load (`scripts/bench/cpu-top.mjs`, flags `--server-node-args
--require=...,--cpu-prof`) showed three avoidable per-request costs (percentages of sampled self time):

| Cost | Where | Share | Fix |
|---|---|--:|---|
| `matchPattern()` built `new RegExp(...)` for every glob on every pricing/capability/thinking-level lookup | `open-sse/providers/pricing.js` | ~16% | compile each glob once (bounded cache keyed by the fixed pattern tables) |
| `new Date().toLocaleTimeString(...)` creates an `Intl.DateTimeFormat` per log line | 5 sites incl. `src/sse/utils/logger.js` | ~5.6% | shared cached formatter, string memoised per wall-clock second (`open-sse/utils/timeString.js`) |
| `fs.statSync(model-catalog.json)` on every catalog lookup (a failing stat when the file is absent) | `open-sse/providers/catalogOverride.js` | ~2.5 - 3.7% | rate limit the mtime check to once per second; `invalidateCatalog()` (called after a sync) still forces an immediate re-read |

Output is byte-identical (verified by `tests/unit/time-string.test.js`, `tests/unit/pricing-match-pattern.test.js`,
`tests/unit/catalog-override-stat-throttle.test.js`; the stat-throttle tests fail against the old code).

A/B, same session, back to back, same machine state, 4 repeats of each cell (before = `runs/pre-fix/ab-before`,
after = `runs/ab-after`):

| Scenario | c | req/s before | req/s after | change | p50 ms | p99 ms | p99.9 ms |
|--|--:|--:|--:|--:|--|--|--|
| chat-ns-plain | 1 | 268 | 445 | +66% | 3.54 -> 2.11 | 8.05 -> 6.20 | 10.26 -> 7.75 |
| chat-ns-plain | 8 | 332 | 486 | +46% | 23.09 -> 15.85 | 35.38 -> 25.65 | 62.97 -> 43.88 |
| chat-ns-plain | 32 | 355 | 519 | **+46%** | 92.32 -> 60.12 | 132.46 -> 91.96 | 167.22 -> 119.45 |
| chat-stream-plain | 1 | 266 | 350 | +32% | 3.60 -> 2.75 | 8.01 -> 6.55 | 9.52 -> 8.43 |
| chat-stream-plain | 8 | 312 | 468 | +50% | 24.74 -> 15.82 | 36.84 -> 41.53 | 59.55 -> 55.03 |
| chat-stream-plain | 32 | 339 | 518 | **+53%** | 98.75 -> 62.48 | 130.21 -> 89.29 | 163.97 -> 113.00 |

The full matrix before (`runs/pre-fix/node-main-*`, taken at `6029c755`) versus after (`runs/node-main-*`) is in the
table below. Those two matrices were run about 70 minutes apart on a shared machine, so the cross-session differences
(+33% to +92% on chat cells; health/models within -8% to +10%, i.e. noise) overstate the gain; **the A/B above (+46% to +53%)
is the number to quote**. Pre-fix saturation was ~320 req/s in the open loop (p99 4.5 s at 320 req/s, client timeouts
at 380 and 420 req/s); post-fix the first unstable rate is 600 req/s with no timeouts.

| Scenario | c | req/s before | req/s after | change | p50 ms before -> after | p99 ms before -> after | p99.9 ms before -> after |
|--|--:|--:|--:|--:|--|--|--|
| health | 32 | 1836 | 1905 | 3.7% | 16.08 -> 15.59 | 31.96 -> 30.50 | 38.69 -> 35.27 |
| models | 32 | 178 | 164 | -7.7% | 172.86 -> 189.51 | 349.22 -> 379.84 | 378.15 -> 402.92 |
| chat-401 | 32 | 998 | 1208 | 21.1% | 30.31 -> 24.91 | 59.37 -> 46.31 | 74.27 -> 56.29 |
| chat-ns-plain | 32 | 355 | 588 | 65.6% | 91.08 -> 55.20 | 122.34 -> 84.02 | 162.38 -> 113.27 |
| chat-stream-plain | 32 | 327 | 501 | 53.2% | 101.50 -> 64.92 | 140.66 -> 91.37 | 185.33 -> 122.41 |
| chat-ns-acl | 32 | 337 | 549 | 63.0% | 98.31 -> 58.10 | 147.60 -> 108.19 | 171.22 -> 149.98 |
| chat-stream-acl | 32 | 298 | 395 | 32.8% | 111.45 -> 78.77 | 145.59 -> 175.55 | 399.01 -> 218.03 |

(The `models` row is unaffected by the change and shows the session noise floor of about ±8%.)

## 6. Cloudflare build: projected cost per 1M requests

**Not measured on Cloudflare.** The Cloudflare build was not run under wrangler/miniflare: a local workerd on this machine would
measure this laptop, not Cloudflare's edge, D1 or Durable Object latency or billing, so any latency figure from it would be
misleading. Instead the projection uses the **exact per-request operation counts** recorded by the counting D1/DO harness
(`tests/unit/cloudflare-d1-ops.test.js`, outputs `docs/benchmarks/d1-ops-before.json` and `d1-ops-after.json`) and Cloudflare's published
prices. Worker CPU and request charges are identical before and after and are excluded from the deltas.

### Prices used (Workers Paid, fetched 2026-10-28)

Sources: [D1 pricing](https://developers.cloudflare.com/d1/platform/pricing/),
[Durable Objects pricing](https://developers.cloudflare.com/durable-objects/platform/pricing/),
[Workers pricing](https://developers.cloudflare.com/workers/platform/pricing/).

| Meter | Included per month | Beyond included |
|---|--:|--:|
| D1 rows read | 25 billion | $0.001 / million |
| D1 rows written | 50 million | $1.00 / million |
| D1 storage | 5 GB | $0.75 / GB-month |
| Durable Object requests (each RPC method call on a stub is one request) | 1 million | $0.15 / million |
| Durable Object duration (128 MB billed regardless of use) | 400,000 GB-s | $12.50 / million GB-s |
| Durable Object SQLite rows read | 25 billion | $0.001 / million |
| Durable Object SQLite rows written | 50 million | $1.00 / million |
| Workers requests | 10 million | $0.30 / million |
| Workers CPU time | 30 million ms | $0.02 / million ms |
| Plan minimum | | $5 / month |

The coordinator Durable Object is `storage = "sqlite"`, so its `storage.get/put/delete` calls bill as rows read/written
at the D1 rates. Where the Durable Object duration is included it uses an **assumed** 1 ms wall-clock per RPC
(not measured); it is below $0.005 per million requests either way.

### Operations and list-price cost per 1M requests

Format: D1 rows read / written · DO RPCs / rows read / rows written, amortised per request.
`scripts/bench/d1-cost.mjs` prints these tables.

| Scenario | Before ops | Before $/1M | After ops | After $/1M | Saving |
|---|---|--:|---|--:|--:|
| warm chat non-stream | 0 / 0.100 · 3 / 7 / 4 | $4.56 | 0.050 / 0.100 · 0.050 / 0.050 / 0.050 | $0.16 | 96.5% |
| warm chat stream | 0 / 0.100 · 3 / 7 / 4 | $4.56 | 0 / 0.100 · 0.050 / 0.050 / 0.050 | $0.16 | 96.5% |
| warm chat, pinned key (force pins + ACL) | 0.500 / 0.100 · 3 / 7 / 4 | $4.56 | 0 / 0.100 · 0.050 / 0.050 / 0.050 | $0.16 | 96.5% |
| warm chat, rate-limited key | 0.500 / 0.100 · 4 / 8 / 5 | $5.71 | 0 / 0.100 · 1.050 / 1.050 / 1.050 | $1.31 | 77.1% |
| warm chat, max-concurrent key | 0.500 / 0.100 · 5 / 9 / 6 | $6.87 | 0 / 0.100 · 2.050 / 2.050 / 2.050 | $2.46 | 64.1% |
| warm chat, budget key | 0 / 0.100 · 5 / 9 / 4 | $4.87 | 0 / 0.100 · 2.050 / 2.050 / 0.050 | $0.46 | 90.5% |
| warm, invalid API key | 20 / 0 · 0 / 0 / 0 | $0.02 | 0 / 0 · 0 / 0 / 0 | $0.00 | 100% |
| warm GET /v1/models | 0 / 0 · 0 / 0 / 0 | $0.00 | 0 / 0 · 0 / 0 / 0 | $0.00 | n/a |
| cold-isolate chat non-stream | 5425 / 0 · 5 / 9 / 4 | $10.19 | 93 / 0 · 3 / 6 / 0 | $0.55 | 94.6% |
| cold-isolate GET /v1/models | 5425 / 0 · 2 / 2 / 0 | $5.73 | 5384 / 0 · 2 / 3 / 0 | $5.69 | 0.7% |
| steady 1 h at 0.1 req/s (360 req) | 0.367 / 0.039 · 6 / 10 / 5 | $5.96 | 0.369 / 0.039 · 2.336 / 5.342 / 0.342 | $0.74 | 87.6% |
| steady 1 h at 1 req/min (60 req) | 2.200 / 0.233 · 6 / 10 / 5 | $6.15 | 2.217 / 0.233 · 3.017 / 6.050 / 1.050 | $1.75 | 71.6% |

(The "cold" rows are the first request in a fresh isolate, which loads the whole working set; "cold chat" falls from 5,425 to 93
D1 rows read because the extreme profile loads only what the request needs. Cold `/v1/models` still reads the whole custom
model table, 5,384 rows; that is the one path the profile does not help.)

Blended chat non-stream cost when a fraction f of requests lands on a cold isolate (cost = f·cold + (1-f)·warm):

| f cold | Before | After | Saving |
|--:|--:|--:|--:|
| 0% | $4.56 | $0.16 | 96.5% |
| 1% | $4.62 | $0.16 | 96.5% |
| 10% | $5.12 | $0.20 | 96.2% |

Where the warm chat money goes, per 1M requests:

| Component | Before | After |
|---|--:|--:|
| D1 rows read | $0.00 | $0.0001 |
| D1 rows written | $0.10 | $0.10 |
| DO requests (RPCs) | $0.45 | $0.0075 |
| DO SQLite rows read | $0.0070 | $0.0001 |
| DO SQLite rows written | $4.00 | $0.05 |
| DO duration (assumed 1 ms/RPC) | $0.0047 | $0.0001 |
| **Total** | **$4.56** | **$0.16** |

### Monthly bill with the included allotments applied

Warm chat non-stream, storage operations only (D1 + coordinator DO), excluding the $5 plan minimum and the identical Workers
request/CPU charges, assuming each meter has its own allotment as the pricing pages list them:

| Requests / month | Before | After |
|--:|--:|--:|
| 1,000,000 | $0.30 | $0.00 |
| 10,000,000 | $4.35 | $0.00 |
| 100,000,000 | $394.85 | $0.60 |
| 1,000,000,000 | $4,449.85 | $57.35 |
| 10,000,000,000 | $45,486.72 | $1,474.85 |

Before, the coordinator exhausted its included 1 million DO requests after 333k requests/month (3 RPCs per request) and the 50 million
included DO written rows after 12.5 million requests/month (4 per request); after, those limits move to 20 million requests/month (0.05 RPC per
request) and 1 billion requests/month (0.05 row per request). The D1 write meter (0.1 row per request) is separate and lasts 500 million requests.
At low volume the saving is therefore smaller in dollars (it is absorbed by the free allotments) and dominant at scale.

## 7. Reproduce

```bash
npm run build                                   # variant node
# one group per command (each takes 5-14 minutes); servers/mock/load run inside the one process tree
scripts/bench/run-all.sh main1                  # health, models, chat-401
scripts/bench/run-all.sh main2                  # chat-ns-plain, chat-stream-plain
scripts/bench/run-all.sh main3                  # chat-ns-acl, chat-stream-acl, open loop 100..400 req/s
scripts/bench/run-all.sh delay logerror knee ab # 50 ms upstream, LOG_LEVEL=ERROR, open loop 450..600, A/B (4 repeats)
AB_NAME=ab-before BENCH_OUT=docs/benchmarks/runs/pre-fix scripts/bench/run-all.sh ab   # same on another build (check out the old commit and rebuild first)

node scripts/bench/report.mjs docs/benchmarks/runs/node-main-1 ... > tables.md   # Appendix A
node scripts/bench/summary.mjs docs/benchmarks/runs/node-main-{1,2,3}              # section 4.1
node scripts/bench/compare.mjs docs/benchmarks/runs/ab-after-dirs...               # before/after
node scripts/bench/d1-cost.mjs                                                     # section 6
node scripts/bench/cpu-top.mjs <file.cpuprofile>                                   # section 5
```

The pre-fix open-loop rates (100, 200, 250, 280, 320 and 300, 340, 380, 420) are recorded in `runs/pre-fix/*/env.json` under `config.openRates` and can be passed with `--open-rates`.
`env.json` records the `HEAD` commit without a dirty flag, so the fixed-build runs name the parent commit `6029c755`.

`run-bench.mjs` flags: `--src <db>` (read-only source, scrubbed copy is used), `--scenarios`, `--concurrency`, `--duration`,
`--warmup`, `--repeats`, `--open-scenario/--open-rates/--open-duration`, `--mock-delay`, `--log-level`, `--server-node-args`
(for `--cpu-prof`), `--smoke`. The sandbox/agent shell gives every command a fresh `/tmp` and kills background processes when it
exits, so each group is a single foreground command; on a normal machine you can run `scripts/bench/run-all.sh` as one job.

## 8. Caveats and what is not verified

* **Shared, non-quiet machine.** A desktop with other applications; load average at the start of the fixed-build runs was 1.7 - 7.3 (1.1 - 3.2 for the pre-fix runs). Between
  sessions chat throughput varied by up to ~13% (`chat-ns-plain` c=32: 519 in the A/B session, 588 in the main-matrix session
  about 14 minutes later, same build). Within a session the repeats agree well (CV 0.4 - 2.8% for most cells; 9.4% for `chat-ns-acl` c=32).
  Treat any difference below ~10% across sessions as noise.
* **Single server process, single machine, loopback.** No network, TLS, reverse proxy, or multi-process/cluster mode was measured.
  Absolute req/s will differ on other hardware; the shape (CPU-bound, N\* ≈ 1 for an instant upstream) will not.
* **Instant mock upstream.** It isolates router overhead. Real providers add their own latency (the 50 ms run shows how
  N\* grows) and real streaming holds connections far longer than the 8-chunk mock.
* **`GET /v1/models` was only measured to c=128** (172 req/s; at c=1024 it would take 6 s per request). Its time is dominated
  by the large custom-model working set in the copied database (6,376 kv rows); not profiled further.
* **p99.9 and max** rest on few samples in several cells: starred where the pooled n is < 10,000. The 15 s open-loop runs give
  4.5k - 27k pooled samples per rate. Maxima are single events and vary run to run (for example a 920 ms excursion in one of three
  400 req/s repeats).
* **Little's law worst case is 13.1%** (`node-main-3` `chat-ns-acl` c=1024 run 1, where the generator's 5 ms sampler was starved while
  1,024 sockets were serviced); every other closed-loop cell agrees within a few percent and most within 0.1%. The sampled
  count is a cross-check of the instrumentation, not an input to any reported latency.
* **Generator not pinned** (an earlier `--gen-cpus` flag was only a label, removed; `env.json` files were corrected to say "unpinned").
  The generator used 0.05 - 0.20 cores, and its send lag p99 in the open loop was 1.0 - 1.8 ms, far below the latencies reported.
* **No Cloudflare measurement.** Section 6 is a projection from exact operation counts and list prices fetched on 2026-10-28; it does not
  include Worker CPU/request charges, D1 or DO storage charges, egress (none for D1), the $5 plan minimum, real isolate recycle rates
  (shown as a sensitivity on f), or real DO wall-clock time (assumed 1 ms/RPC, under $0.005 per million). Cloudflare may change prices; edit
  `PRICE` in `scripts/bench/d1-cost.mjs`.
* **CPU and RSS figures** are sampled over the whole run (connection barrier, warm-up, window and drain), not only the measured window. At saturation
  that is immaterial (1.15 cores), but at low rates the average is diluted, so do not divide low-rate CPU by rate to get a CPU cost per request;
  the 1.86 ms per request quoted above is the saturated figure. RSS assumes 4 KiB pages.
* **"After" Cloudflare operation counts are amortised over the harness's 20-request warm scenarios** (one coalesced coordinator flush per 20
  requests, `NINEROUTER_COORDINATOR_FLUSH_MS` is 30 s in production). At several hundred req/s a flush covers thousands of requests and the
  per-request cost falls further; at 1 request/minute it is higher (the steady-state rows: 3.0 RPC per request).
* **Open-loop sockets are created lazily** (no connection barrier as in the closed loop), and the 2 s warm-up absorbs most of that; the occasional
  excursions in single repeats (for example 920 ms at 400 req/s) were not attributed to the server or to connection setup.
* **Profile percentages** in section 5 are from two single runs (`scripts/bench/cpu-top.mjs`); the `.cpuprofile` files (1.7 MB each) are not committed. The summed
  25% self-time predicts about +33% throughput versus the measured +46%; the remainder is plausibly GC pressure from the removed allocations, not measured.
* **The bench database copy** (`setup-bench-db.mjs`) is scrubbed by key name only and lives under the system temp directory while a run is in flight; run it on
  a trusted machine. Committed artefacts were grepped for `sk-` strings and bearer tokens and contain none.
* **Pre-fix vs post-fix cross-session comparison** overstates the gain; use the A/B (section 5).
* **Not tested:** the Cloudflare/Worker runtime under load, memory over hours (RSS was observed only within each run, roughly 0.57 MB
  per extra in-flight connection), more than 1,024 connections, mixed workloads, real upstreams, and any dashboard route.

---

## Appendix A: all generated tables

Produced by `node scripts/bench/report.mjs <run dirs>`. For each scenario: per-run table (mean ± s over 3 repeats; throughput with 95% CI),
the pooled exact percentiles with the cold-connect burst, the Little's law table and the knee analysis.


### Run `node-main-1`

commit 6029c755, v24.22.0, AMD Ryzen 7 5800H with Radeon Graphics (16 logical CPUs), build variant node; server pinned to CPUs 0-3, mock upstream 4-5 (delay 0ms), generator unpinned (shares the machine with whatever else is running); server log level INFO (default); 3 repeats x 8s measured after 2s warm-up.

#### health - closed loop

| c | req/s (mean ±95% CI) | err % | p50 ms | p90 ms | p95 ms | p99 ms | p99.9 ms | max ms | n/run | srv CPU cores | srv RSS peak MB | gen CPU |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 1 | 1486 ±419 | 0.000 | 0.60 ±0.05 | 0.81 ±0.17 | 0.98 ±0.25 | 1.62 ±0.54 | 6.86 ±0.63 | 27.4 | 11889 | 1.11 | 524 | 0.17 |
| 8 | 1816 ±151 | 0.000 | 3.67 ±0.14 | 7.05 ±0.23 | 8.06 ±0.58 | 11.26 ±1.55 | 21.50 ±5.10 | 56.4 | 14530 | 1.16 | 525 | 0.16 |
| 32 | 1905 ±71 | 0.000 | 15.56 ±0.23 | 21.60 ±0.38 | 22.41 ±0.37 | 29.32 ±2.18 | 34.40 ±1.13 | 49.5 | 15238 | 1.16 | 537 | 0.17 |
| 128 | 1901 ±82 | 0.000 | 67.63 ±0.65 | 72.70 ±2.39 | 75.62 ±1.68 | 90.73 ±3.02 | 101.97 ±5.32 | 141.0 | 15205 | 1.16 | 541 | 0.17 |
| 512 | 1931 ±36 | 0.000 | 260.76 ±1.20 | 283.04 ±4.59 | 289.26 ±9.81 | 298.96 ±15.88 | 309.12 ±15.18 | 329.1 | 15451 | 1.16 | 554 | 0.18 |
| 1024 | 1924 ±23 | 0.000 | 521.35 ±5.53 | 570.30 ±19.43 | 596.69 ±14.37 | 605.09 ±14.00 | 619.06 ±24.33 | 640.9 | 15392 | 1.19 | 938 | 0.18 |

`*` p99.9 rests on fewer than 10,000 samples per run (the top 0.1% is a handful of requests); treat it as indicative. `max` is the largest value over all repeats.

Exact percentiles over the pooled samples of all repeats of each cell (nearest rank on the full sorted set):

| c | pooled n | mean ms | stddev ms | p50 | p90 | p95 | p99 | p99.9 | max | cold-connect burst: all c sockets answered after (ms) | slowest first response (ms) |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 1 | 35667 | 0.67 | 0.53 | 0.59 | 0.81 | 0.99 | 1.74 | 6.72 | 27.4 | 2 | 3 |
| 8 | 43590 | 4.41 | 1.95 | 3.68 | 6.97 | 8.23 | 11.54 | 20.44 | 56.4 | 6 | 7 |
| 32 | 45715 | 16.80 | 2.96 | 15.59 | 21.58 | 22.43 | 30.50 | 35.27 | 49.5 | 19 | 19 |
| 128 | 45615 | 67.35 | 5.57 | 67.56 | 73.55 | 75.81 | 90.35 | 101.91 | 141.0 | 84 | 101 |
| 512 | 46354 | 265.14 | 11.54 | 260.45 | 281.80 | 287.63 | 308.98 | 323.17 | 329.1 | 305 | 340 |
| 1024 | 46175 | 531.73 | 25.52 | 521.42 | 571.85 | 595.24 | 615.09 | 629.99 | 640.9 | 583 | 588 |

`*` pooled n < 10,000: p99.9 rests on fewer than 10 samples.

| c | lambda req/s | W mean s | L = lambda*W | L sampled in flight | L error % | c (closed-loop population) |
|--:|--:|--:|--:|--:|--:|--:|
| 1 | 1486.1 | 0.00068 | 1.01 | 1.00 | 0.76 | 1 |
| 8 | 1816.3 | 0.00441 | 8.01 | 8.00 | 0.09 | 8 |
| 32 | 1904.8 | 0.01680 | 32.00 | 32.00 | -0.01 | 32 |
| 128 | 1900.6 | 0.06737 | 128.04 | 128.00 | 0.03 | 128 |
| 512 | 1931.4 | 0.26515 | 512.11 | 512.00 | 0.02 | 512 |
| 1024 | 1924.0 | 0.53173 | 1023.03 | 1024.00 | -0.09 | 1024 |

- peak throughput lambda_max = **1931 req/s** (mean over repeats)
- unloaded mean latency W(c=1) = 0.678 ms, so the saturation population N* = lambda_max x W(1) = **1.3** concurrent requests
- smallest tested concurrency reaching 95% of lambda_max: **c = 32**
- highest tested concurrency with zero errors in every repeat: **c = 1024**
- run-to-run variance of throughput (coefficient of variation): c=1: 11.3%, c=8: 3.3%, c=32: 1.5%, c=128: 1.7%, c=512: 0.7%, c=1024: 0.5%

#### models - closed loop

| c | req/s (mean ±95% CI) | err % | p50 ms | p90 ms | p95 ms | p99 ms | p99.9 ms | max ms | n/run | srv CPU cores | srv RSS peak MB | gen CPU |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 1 | 161 ±1 | 0.000 | 6.05 ±0.02 | 6.46 ±0.05 | 6.71 ±0.07 | 11.86 ±0.06 | 13.25 ±0.81* | 22.4 | 1291 | 1.02 | 493 | 0.05 |
| 8 | 163 ±5 | 0.000 | 43.26 ±1.20 | 80.06 ±0.70 | 85.10 ±1.87 | 96.91 ±2.51 | 110.94 ±1.89* | 119.0 | 1304 | 1.06 | 511 | 0.05 |
| 32 | 164 ±9 | 0.000 | 189.64 ±4.15 | 201.98 ±6.86 | 209.62 ±6.22 | 376.52 ±8.45 | 399.43 ±7.12* | 418.4 | 1315 | 1.06 | 509 | 0.04 |
| 128 | 172 ±4 | 0.000 | 746.65 ±5.36 | 762.22 ±10.57 | 767.33 ±11.29 | 774.62 ±10.88 | 780.67 ±8.19* | 790.2 | 1373 | 1.03 | 510 | 0.05 |

`*` p99.9 rests on fewer than 10,000 samples per run (the top 0.1% is a handful of requests); treat it as indicative. `max` is the largest value over all repeats.

Exact percentiles over the pooled samples of all repeats of each cell (nearest rank on the full sorted set):

| c | pooled n | mean ms | stddev ms | p50 | p90 | p95 | p99 | p99.9 | max | cold-connect burst: all c sockets answered after (ms) | slowest first response (ms) |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 1 | 3874 | 6.20 | 0.95 | 6.05 | 6.46 | 6.72 | 11.84 | 13.97* | 22.4 | 14 | 18 |
| 8 | 3913 | 49.03 | 14.59 | 43.15 | 80.40 | 84.66 | 97.03 | 112.85* | 119.0 | 50 | 54 |
| 32 | 3944 | 194.87 | 30.89 | 189.51 | 203.45 | 211.78 | 379.84 | 402.92* | 418.4 | 194 | 199 |
| 128 | 4118 | 745.59 | 17.17 | 746.14 | 765.48 | 772.10 | 781.46 | 789.59* | 790.2 | 776 | 807 |

`*` pooled n < 10,000: p99.9 rests on fewer than 10 samples.

| c | lambda req/s | W mean s | L = lambda*W | L sampled in flight | L error % | c (closed-loop population) |
|--:|--:|--:|--:|--:|--:|--:|
| 1 | 161.4 | 0.00620 | 1.00 | 1.00 | 0.00 | 1 |
| 8 | 163.0 | 0.04904 | 8.00 | 8.00 | -0.06 | 8 |
| 32 | 164.3 | 0.19494 | 32.04 | 32.00 | 0.11 | 32 |
| 128 | 171.6 | 0.74565 | 127.94 | 128.00 | -0.05 | 128 |

- peak throughput lambda_max = **172 req/s** (mean over repeats)
- unloaded mean latency W(c=1) = 6.195 ms, so the saturation population N* = lambda_max x W(1) = **1.1** concurrent requests
- smallest tested concurrency reaching 95% of lambda_max: **c = 8**
- highest tested concurrency with zero errors in every repeat: **c = 128**
- run-to-run variance of throughput (coefficient of variation): c=1: 0.3%, c=8: 1.3%, c=32: 2.3%, c=128: 1.0%

#### chat-401 - closed loop

| c | req/s (mean ±95% CI) | err % | p50 ms | p90 ms | p95 ms | p99 ms | p99.9 ms | max ms | n/run | srv CPU cores | srv RSS peak MB | gen CPU |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 1 | 1095 ±153 | 0.000 | 0.85 ±0.04 | 0.99 ±0.09 | 1.09 ±0.11 | 1.65 ±0.30 | 6.18 ±0.30* | 21.8 | 8759 | 1.11 | 560 | 0.13 |
| 8 | 1204 ±44 | 0.000 | 5.51 ±0.07 | 10.77 ±0.17 | 11.25 ±0.12 | 16.30 ±0.70 | 23.45 ±5.45* | 40.3 | 9635 | 1.14 | 559 | 0.13 |
| 32 | 1208 ±36 | 0.000 | 25.00 ±0.40 | 31.18 ±0.33 | 32.69 ±0.95 | 45.86 ±1.81 | 56.23 ±0.65* | 78.5 | 9662 | 1.13 | 562 | 0.13 |
| 128 | 1225 ±38 | 0.000 | 103.43 ±1.12 | 111.04 ±2.61 | 114.95 ±3.83 | 126.44 ±1.21 | 134.66 ±0.56* | 201.1 | 9801 | 1.13 | 561 | 0.13 |
| 512 | 1235 ±13 | 0.000 | 412.07 ±1.68 | 431.09 ±1.58 | 443.16 ±5.63 | 451.00 ±5.86 | 454.59 ±8.34* | 466.3 | 9883 | 1.13 | 602 | 0.14 |
| 1024 | 1212 ±43 | 0.000 | 832.73 ±11.15 | 899.02 ±8.76 | 918.73 ±30.28 | 932.26 ±35.69 | 934.53 ±35.35* | 974.2 | 9698 | 1.14 | 851 | 0.16 |

`*` p99.9 rests on fewer than 10,000 samples per run (the top 0.1% is a handful of requests); treat it as indicative. `max` is the largest value over all repeats.

Exact percentiles over the pooled samples of all repeats of each cell (nearest rank on the full sorted set):

| c | pooled n | mean ms | stddev ms | p50 | p90 | p95 | p99 | p99.9 | max | cold-connect burst: all c sockets answered after (ms) | slowest first response (ms) |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 1 | 26277 | 0.91 | 0.50 | 0.84 | 1.00 | 1.10 | 1.76 | 6.24 | 21.8 | 2 | 2 |
| 8 | 28904 | 6.64 | 2.50 | 5.52 | 10.78 | 11.26 | 16.29 | 21.90 | 40.3 | 8 | 8 |
| 32 | 28986 | 26.49 | 4.06 | 24.91 | 31.12 | 32.72 | 46.31 | 56.29 | 78.5 | 26 | 27 |
| 128 | 29404 | 104.45 | 5.95 | 103.32 | 110.93 | 116.01 | 126.99 | 134.72 | 201.1 | 114 | 125 |
| 512 | 29648 | 414.84 | 11.34 | 412.09 | 430.39 | 441.40 | 453.71 | 457.32 | 466.3 | 452 | 474 |
| 1024 | 29095 | 844.70 | 33.82 | 833.73 | 893.59 | 905.51 | 961.42 | 973.22 | 974.2 | 900 | 923 |

`*` pooled n < 10,000: p99.9 rests on fewer than 10 samples.

| c | lambda req/s | W mean s | L = lambda*W | L sampled in flight | L error % | c (closed-loop population) |
|--:|--:|--:|--:|--:|--:|--:|
| 1 | 1094.9 | 0.00091 | 1.00 | 1.00 | 0.12 | 1 |
| 8 | 1204.3 | 0.00664 | 8.00 | 8.00 | 0.00 | 8 |
| 32 | 1207.8 | 0.02650 | 32.00 | 32.00 | 0.00 | 32 |
| 128 | 1225.2 | 0.10447 | 127.99 | 128.00 | -0.01 | 128 |
| 512 | 1235.3 | 0.41484 | 512.47 | 512.00 | 0.09 | 512 |
| 1024 | 1212.3 | 0.84480 | 1024.14 | 1024.00 | 0.01 | 1024 |

- peak throughput lambda_max = **1235 req/s** (mean over repeats)
- unloaded mean latency W(c=1) = 0.914 ms, so the saturation population N* = lambda_max x W(1) = **1.1** concurrent requests
- smallest tested concurrency reaching 95% of lambda_max: **c = 8**
- highest tested concurrency with zero errors in every repeat: **c = 1024**
- run-to-run variance of throughput (coefficient of variation): c=1: 5.6%, c=8: 1.5%, c=32: 1.2%, c=128: 1.3%, c=512: 0.4%, c=1024: 1.4%


### Run `node-main-2`

commit 6029c755, v24.22.0, AMD Ryzen 7 5800H with Radeon Graphics (16 logical CPUs), build variant node; server pinned to CPUs 0-3, mock upstream 4-5 (delay 0ms), generator unpinned (shares the machine with whatever else is running); server log level INFO (default); 3 repeats x 8s measured after 2s warm-up.

#### chat-ns-plain - closed loop

| c | req/s (mean ±95% CI) | err % | p50 ms | p90 ms | p95 ms | p99 ms | p99.9 ms | max ms | n/run | srv CPU cores | srv RSS peak MB | gen CPU |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 1 | 436 ±96 | 0.000 | 2.18 ±0.21 | 2.68 ±0.20 | 2.91 ±0.21 | 6.20 ±0.35 | 7.99 ±0.77* | 25.4 | 3488 | 1.08 | 593 | 0.08 |
| 8 | 543 ±39 | 0.000 | 13.98 ±0.32 | 18.12 ±0.45 | 19.21 ±0.42 | 22.45 ±1.75 | 46.14 ±5.90* | 60.9 | 4346 | 1.12 | 596 | 0.07 |
| 32 | 588 ±22 | 0.000 | 55.23 ±0.50 | 65.25 ±1.22 | 67.65 ±1.73 | 80.38 ±9.62 | 110.30 ±5.61* | 117.2 | 4706 | 1.13 | 637 | 0.06 |
| 128 | 617 ±10 | 0.000 | 191.56 ±0.73 | 250.49 ±1.04 | 257.20 ±3.18 | 281.00 ±16.00 | 331.60 ±9.37* | 342.5 | 4935 | 1.15 | 661 | 0.06 |
| 512 | 604 ±41 | 0.000 | 791.70 ±9.32 | 1053.69 ±10.84 | 1080.20 ±14.84 | 1116.81 ±11.18 | 1139.78 ±8.30* | 1148.1 | 4829 | 1.21 | 872 | 0.07 |
| 1024 | 601 ±63 | 0.000 | 1664.06 ±53.56 | 1893.22 ±204.20 | 1923.83 ±249.44 | 1959.74 ±277.53 | 1972.02 ±281.46* | 2295.6 | 4810 | 1.21 | 1178 | 0.07 |

`*` p99.9 rests on fewer than 10,000 samples per run (the top 0.1% is a handful of requests); treat it as indicative. `max` is the largest value over all repeats.

Exact percentiles over the pooled samples of all repeats of each cell (nearest rank on the full sorted set):

| c | pooled n | mean ms | stddev ms | p50 | p90 | p95 | p99 | p99.9 | max | cold-connect burst: all c sockets answered after (ms) | slowest first response (ms) |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 1 | 10463 | 2.29 | 0.73 | 2.19 | 2.72 | 2.94 | 6.11 | 8.25 | 25.4 | 4 | 4 |
| 8 | 13038 | 14.72 | 2.66 | 13.95 | 18.15 | 19.18 | 22.58 | 45.71 | 60.9 | 29 | 57 |
| 32 | 14118 | 54.44 | 9.51 | 55.20 | 65.21 | 67.45 | 84.02 | 113.27 | 117.2 | 59 | 68 |
| 128 | 14805 | 207.12 | 30.79 | 191.64 | 250.66 | 256.41 | 290.33 | 342.20 | 342.5 | 224 | 251 |
| 512 | 14486 | 837.97 | 107.64 | 793.66 | 1058.31 | 1077.64 | 1123.03 | 1145.50 | 1148.1 | 914 | 943 |
| 1024 | 14429 | 1700.14 | 130.12 | 1685.46 | 1820.54 | 2076.47 | 2210.29 | 2293.46 | 2295.6 | 1802 | 1850 |

`*` pooled n < 10,000: p99.9 rests on fewer than 10 samples.

| c | lambda req/s | W mean s | L = lambda*W | L sampled in flight | L error % | c (closed-loop population) |
|--:|--:|--:|--:|--:|--:|--:|
| 1 | 436.0 | 0.00230 | 1.00 | 1.00 | 0.43 | 1 |
| 8 | 543.3 | 0.01473 | 8.00 | 8.00 | 0.03 | 8 |
| 32 | 588.3 | 0.05444 | 32.03 | 32.00 | 0.09 | 32 |
| 128 | 616.9 | 0.20713 | 127.77 | 128.00 | -0.18 | 128 |
| 512 | 603.6 | 0.83800 | 505.80 | 512.00 | -1.21 | 512 |
| 1024 | 601.2 | 1.70043 | 1022.31 | 1024.00 | -0.16 | 1024 |

- peak throughput lambda_max = **617 req/s** (mean over repeats)
- unloaded mean latency W(c=1) = 2.304 ms, so the saturation population N* = lambda_max x W(1) = **1.4** concurrent requests
- smallest tested concurrency reaching 95% of lambda_max: **c = 32**
- highest tested concurrency with zero errors in every repeat: **c = 1024**
- run-to-run variance of throughput (coefficient of variation): c=1: 8.9%, c=8: 2.9%, c=32: 1.5%, c=128: 0.6%, c=512: 2.7%, c=1024: 4.2%

#### chat-stream-plain - closed loop

| c | req/s (mean ±95% CI) | err % | p50 ms | p90 ms | p95 ms | p99 ms | p99.9 ms | max ms | n/run | srv CPU cores | srv RSS peak MB | gen CPU |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 1 | 385 ±46 | 0.000 | 2.46 ±0.15 | 3.05 ±0.08 | 3.37 ±0.14 | 6.35 ±0.33 | 7.81 ±0.55* | 23.1 | 3083 | 1.11 | 590 | 0.09 |
| 8 | 478 ±63 | 0.000 | 16.00 ±0.89 | 19.79 ±0.96 | 20.81 ±0.98 | 25.02 ±2.44 | 51.04 ±4.19* | 55.2 | 3827 | 1.11 | 596 | 0.09 |
| 32 | 501 ±18 | 0.000 | 65.02 ±1.03 | 74.38 ±0.83 | 76.99 ±1.33 | 90.79 ±2.83 | 118.41 ±9.33* | 129.3 | 4005 | 1.12 | 645 | 0.07 |
| 128 | 513 ±33 | 0.000 | 255.64 ±4.33 | 295.32 ±19.34 | 308.80 ±20.18 | 362.48 ±43.99 | 369.52 ±44.79* | 420.9 | 4106 | 1.15 | 659 | 0.08 |
| 512 | 523 ±26 | 0.000 | 950.07 ±23.00 | 1204.08 ±57.07 | 1243.20 ±59.62 | 1343.10 ±112.09 | 1348.40 ±111.71* | 1438.0 | 4185 | 1.19 | 827 | 0.08 |
| 1024 | 530 ±36 | 0.000 | 1860.60 ±84.56 | 2210.73 ±183.18 | 2322.04 ±70.34 | 2346.91 ±82.92 | 2363.08 ±93.93* | 2422.5 | 4243 | 1.19 | 1264 | 0.08 |

`*` p99.9 rests on fewer than 10,000 samples per run (the top 0.1% is a handful of requests); treat it as indicative. `max` is the largest value over all repeats.

Exact percentiles over the pooled samples of all repeats of each cell (nearest rank on the full sorted set):

| c | pooled n | mean ms | stddev ms | p50 | p90 | p95 | p99 | p99.9 | max | cold-connect burst: all c sockets answered after (ms) | slowest first response (ms) |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 1 | 9248 | 2.59 | 0.70 | 2.47 | 3.05 | 3.36 | 6.30 | 7.74* | 23.1 | 4 | 4 |
| 8 | 11480 | 16.72 | 2.74 | 15.98 | 19.73 | 20.97 | 24.87 | 50.95 | 55.2 | 16 | 18 |
| 32 | 12015 | 63.90 | 9.83 | 64.92 | 74.36 | 77.28 | 91.37 | 122.41 | 129.3 | 70 | 87 |
| 128 | 12318 | 250.50 | 36.16 | 255.54 | 291.52 | 316.77 | 349.50 | 418.60 | 420.9 | 254 | 267 |
| 512 | 12554 | 997.12 | 140.70 | 952.97 | 1185.34 | 1266.06 | 1383.00 | 1437.81 | 1438.0 | 1008 | 1079 |
| 1024 | 12730 | 1957.89 | 212.54 | 1929.91 | 2283.88 | 2325.74 | 2391.19 | 2413.87 | 2422.5 | 2016 | 2068 |

`*` pooled n < 10,000: p99.9 rests on fewer than 10 samples.

| c | lambda req/s | W mean s | L = lambda*W | L sampled in flight | L error % | c (closed-loop population) |
|--:|--:|--:|--:|--:|--:|--:|
| 1 | 385.3 | 0.00260 | 1.00 | 1.00 | 0.12 | 1 |
| 8 | 478.3 | 0.01676 | 8.01 | 8.00 | 0.18 | 8 |
| 32 | 500.6 | 0.06390 | 31.99 | 32.00 | -0.03 | 32 |
| 128 | 513.3 | 0.25061 | 128.63 | 128.00 | 0.49 | 128 |
| 512 | 523.1 | 0.99745 | 521.75 | 512.00 | 1.90 | 512 |
| 1024 | 530.4 | 1.95793 | 1038.52 | 1024.00 | 1.42 | 1024 |

- peak throughput lambda_max = **530 req/s** (mean over repeats)
- unloaded mean latency W(c=1) = 2.598 ms, so the saturation population N* = lambda_max x W(1) = **1.4** concurrent requests
- smallest tested concurrency reaching 95% of lambda_max: **c = 128**
- highest tested concurrency with zero errors in every repeat: **c = 1024**
- run-to-run variance of throughput (coefficient of variation): c=1: 4.8%, c=8: 5.3%, c=32: 1.4%, c=128: 2.6%, c=512: 2.0%, c=1024: 2.8%


### Run `node-main-3`

commit 6029c755, v24.22.0, AMD Ryzen 7 5800H with Radeon Graphics (16 logical CPUs), build variant node; server pinned to CPUs 0-3, mock upstream 4-5 (delay 0ms), generator unpinned (shares the machine with whatever else is running); server log level INFO (default); 3 repeats x 8s measured after 2s warm-up.

#### chat-ns-acl - closed loop

| c | req/s (mean ±95% CI) | err % | p50 ms | p90 ms | p95 ms | p99 ms | p99.9 ms | max ms | n/run | srv CPU cores | srv RSS peak MB | gen CPU |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 1 | 431 ±77 | 0.000 | 2.21 ±0.17 | 2.70 ±0.17 | 2.93 ±0.19 | 5.98 ±0.29 | 7.36 ±0.15* | 31.5 | 3450 | 1.08 | 599 | 0.08 |
| 8 | 515 ±90 | 0.000 | 14.63 ±0.63 | 19.11 ±1.77 | 21.00 ±3.52 | 27.66 ±7.66 | 51.29 ±4.88* | 57.3 | 4117 | 1.11 | 603 | 0.07 |
| 32 | 549 ±128 | 0.000 | 59.40 ±4.52 | 72.34 ±11.56 | 76.88 ±16.35 | 95.78 ±29.36 | 126.01 ±31.15* | 163.9 | 4394 | 1.11 | 644 | 0.06 |
| 128 | 511 ±60 | 0.000 | 235.16 ±14.51 | 316.89 ±29.70 | 339.50 ±44.16 | 391.27 ±66.83 | 423.57 ±75.05* | 507.4 | 4087 | 1.13 | 668 | 0.06 |
| 512 | 483 ±67 | 0.000 | 974.12 ±52.91 | 1343.04 ±89.89 | 1470.63 ±165.07 | 1566.81 ±92.95 | 1653.87 ±186.24* | 1864.1 | 3862 | 1.17 | 902 | 0.07 |
| 1024 | 497 ±63 | 0.000 | 2108.10 ±311.84 | 2390.42 ±634.09 | 2397.35 ±636.11 | 2797.99 ±1321.42 | 2853.22 ±1294.54* | 4335.3 | 3979 | 1.17 | 1282 | 0.06 |

`*` p99.9 rests on fewer than 10,000 samples per run (the top 0.1% is a handful of requests); treat it as indicative. `max` is the largest value over all repeats.

Exact percentiles over the pooled samples of all repeats of each cell (nearest rank on the full sorted set):

| c | pooled n | mean ms | stddev ms | p50 | p90 | p95 | p99 | p99.9 | max | cold-connect burst: all c sockets answered after (ms) | slowest first response (ms) |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 1 | 10350 | 2.32 | 0.71 | 2.20 | 2.73 | 2.97 | 5.92 | 7.22 | 31.5 | 4 | 4 |
| 8 | 12352 | 15.55 | 3.46 | 14.47 | 18.79 | 20.36 | 29.86 | 54.13 | 57.3 | 15 | 16 |
| 32 | 13183 | 58.31 | 12.83 | 58.10 | 69.99 | 77.49 | 108.19 | 149.98 | 163.9 | 56 | 66 |
| 128 | 12261 | 250.08 | 48.94 | 230.15 | 313.26 | 341.17 | 402.07 | 471.03 | 507.4 | 259 | 320 |
| 512 | 11587 | 1048.10 | 189.52 | 978.60 | 1345.30 | 1426.32 | 1642.38 | 1860.69 | 1864.1 | 1066 | 1152 |
| 1024 | 11938 | 2114.15 | 405.77 | 1939.30 | 2518.15 | 3096.13 | 4295.49 | 4323.83 | 4335.3 | 2072 | 2307 |

`*` pooled n < 10,000: p99.9 rests on fewer than 10 samples.

| c | lambda req/s | W mean s | L = lambda*W | L sampled in flight | L error % | c (closed-loop population) |
|--:|--:|--:|--:|--:|--:|--:|
| 1 | 431.3 | 0.00232 | 1.00 | 1.00 | 0.24 | 1 |
| 8 | 514.7 | 0.01560 | 8.03 | 8.00 | 0.35 | 8 |
| 32 | 549.3 | 0.05868 | 32.23 | 32.00 | 0.72 | 32 |
| 128 | 510.9 | 0.25047 | 127.96 | 128.00 | -0.03 | 128 |
| 512 | 482.8 | 1.04990 | 506.88 | 512.00 | -1.00 | 512 |
| 1024 | 497.4 | 2.12441 | 1056.72 | 1024.00 | 3.20 | 1024 |

- peak throughput lambda_max = **549 req/s** (mean over repeats)
- unloaded mean latency W(c=1) = 2.324 ms, so the saturation population N* = lambda_max x W(1) = **1.3** concurrent requests
- smallest tested concurrency reaching 95% of lambda_max: **c = 32**
- highest tested concurrency with zero errors in every repeat: **c = 1024**
- run-to-run variance of throughput (coefficient of variation): c=1: 7.2%, c=8: 7.0%, c=32: 9.4%, c=128: 4.7%, c=512: 5.6%, c=1024: 5.1%

#### chat-stream-acl - closed loop

| c | req/s (mean ±95% CI) | err % | p50 ms | p90 ms | p95 ms | p99 ms | p99.9 ms | max ms | n/run | srv CPU cores | srv RSS peak MB | gen CPU |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 1 | 342 ±48 | 0.000 | 2.74 ±0.08 | 3.52 ±0.40 | 4.04 ±0.74 | 7.21 ±1.20 | 9.31 ±2.84* | 14.1 | 2734 | 1.09 | 601 | 0.08 |
| 8 | 372 ±41 | 0.000 | 20.38 ±0.86 | 25.35 ±1.30 | 28.20 ±2.12 | 36.58 ±4.53 | 57.59 ±2.50* | 70.1 | 2978 | 1.09 | 621 | 0.08 |
| 32 | 395 ±81 | 0.000 | 79.29 ±4.67 | 100.11 ±14.44 | 111.02 ±24.87 | 160.86 ±49.70 | 184.19 ±62.90* | 227.0 | 3162 | 1.10 | 636 | 0.07 |
| 128 | 427 ±90 | 0.000 | 296.57 ±19.59 | 376.09 ±25.80 | 404.98 ±30.27 | 470.34 ±49.42 | 497.09 ±48.66* | 553.9 | 3417 | 1.12 | 669 | 0.07 |
| 512 | 497 ±12 | 0.000 | 961.10 ±15.96 | 1230.88 ±20.47 | 1269.57 ±51.34 | 1324.17 ±40.76 | 1334.70 ±47.79* | 1375.8 | 3974 | 1.18 | 893 | 0.07 |
| 1024 | 503 ±54 | 0.000 | 1995.07 ±47.08 | 2238.75 ±219.30 | 2268.37 ±226.66 | 2320.75 ±262.05 | 2328.96 ±257.01* | 2613.7 | 4115 | 1.18 | 1241 | 0.07 |

`*` p99.9 rests on fewer than 10,000 samples per run (the top 0.1% is a handful of requests); treat it as indicative. `max` is the largest value over all repeats.

Exact percentiles over the pooled samples of all repeats of each cell (nearest rank on the full sorted set):

| c | pooled n | mean ms | stddev ms | p50 | p90 | p95 | p99 | p99.9 | max | cold-connect burst: all c sockets answered after (ms) | slowest first response (ms) |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 1 | 8202 | 2.92 | 0.84 | 2.74 | 3.47 | 4.02 | 6.90 | 11.07* | 14.1 | 4 | 4 |
| 8 | 8935 | 21.48 | 3.98 | 20.36 | 25.32 | 27.78 | 38.12 | 56.60* | 70.1 | 22 | 23 |
| 32 | 9485 | 80.86 | 20.42 | 78.77 | 97.78 | 111.59 | 175.55 | 218.03* | 227.0 | 100 | 144 |
| 128 | 10251 | 299.23 | 58.71 | 290.01 | 377.76 | 407.34 | 466.07 | 540.02 | 553.9 | 411 | 511 |
| 512 | 11921 | 1026.65 | 133.92 | 957.74 | 1216.24 | 1260.86 | 1339.83 | 1371.86 | 1375.8 | 1052 | 1137 |
| 1024 | 12344 | 2043.10 | 163.48 | 2017.04 | 2365.60 | 2437.26 | 2530.65 | 2612.80 | 2613.7 | 2144 | 2208 |

`*` pooled n < 10,000: p99.9 rests on fewer than 10 samples.

| c | lambda req/s | W mean s | L = lambda*W | L sampled in flight | L error % | c (closed-loop population) |
|--:|--:|--:|--:|--:|--:|--:|
| 1 | 341.8 | 0.00293 | 1.00 | 1.00 | 0.17 | 1 |
| 8 | 372.3 | 0.02151 | 8.01 | 8.00 | 0.10 | 8 |
| 32 | 395.2 | 0.08123 | 32.10 | 32.00 | 0.32 | 32 |
| 128 | 427.1 | 0.30062 | 128.40 | 128.00 | 0.31 | 128 |
| 512 | 496.7 | 1.02670 | 509.97 | 512.00 | -0.40 | 512 |
| 1024 | 503.0 | 2.04398 | 1028.19 | 1024.00 | 0.41 | 1024 |

- peak throughput lambda_max = **503 req/s** (mean over repeats)
- unloaded mean latency W(c=1) = 2.931 ms, so the saturation population N* = lambda_max x W(1) = **1.5** concurrent requests
- smallest tested concurrency reaching 95% of lambda_max: **c = 512**
- highest tested concurrency with zero errors in every repeat: **c = 1024**
- run-to-run variance of throughput (coefficient of variation): c=1: 5.6%, c=8: 4.5%, c=32: 8.3%, c=128: 8.4%, c=512: 1.0%, c=1024: 4.3%

#### open loop (constant arrival rate, latency measured from the intended send time)

| scenario | target req/s | achieved req/s | err % | dropped | p50 ms | p90 ms | p95 ms | p99 ms | p99.9 ms | max ms | n/run | generator send lag p99 ms | srv CPU cores |
|--|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| chat-ns-plain | 100 | 100.0 ±0.0 | 0.000 | 0 | 3.65 ±0.05 | 4.24 ±0.02 | 4.47 ±0.03 | 7.35 ±0.45 | 12.06 ±4.42* | 22.0 | 1500 | 1.06 | 0.33 |
| chat-ns-plain | 200 | 200.0 ±0.0 | 0.000 | 0 | 3.31 ±0.07 | 4.02 ±0.06 | 4.39 ±0.13 | 7.33 ±0.38 | 22.42 ±11.27* | 40.6 | 3000 | 1.06 | 0.59 |
| chat-ns-plain | 250 | 250.0 ±0.0 | 0.000 | 0 | 3.12 ±0.10 | 3.93 ±0.29 | 4.48 ±0.41 | 8.14 ±0.82 | 38.98 ±3.13* | 44.4 | 3750 | 1.14 | 0.66 |
| chat-ns-plain | 300 | 300.0 ±0.0 | 0.000 | 0 | 3.02 ±0.03 | 3.93 ±0.03 | 5.76 ±0.59 | 10.61 ±2.73 | 51.34 ±3.78* | 57.8 | 4500 | 1.47 | 0.73 |
| chat-ns-plain | 350 | 350.0 ±0.0 | 0.000 | 0 | 2.89 ±0.04 | 4.16 ±0.30 | 7.16 ±0.37 | 24.54 ±6.20 | 76.04 ±10.70* | 98.7 | 5250 | 1.44 | 0.81 |
| chat-ns-plain | 400 | 400.0 ±0.0 | 0.000 | 0 | 2.98 ±0.05 | 11.02 ±6.79 | 20.77 ±15.72 | 245.04 ±358.87 | 384.38 ±443.35* | 920.7 | 6000 | 1.49 | 0.92 |

Open loop, exact percentiles over the pooled samples of all repeats (latency from the intended send time):

| target req/s | pooled n | p50 | p90 | p95 | p99 | p99.9 | max |
|--:|--:|--:|--:|--:|--:|--:|--:|
| 100 | 4500 | 3.65 | 4.24 | 4.48 | 7.34 | 10.87* | 22.0 |
| 200 | 9000 | 3.31 | 4.02 | 4.38 | 7.34 | 25.42* | 40.6 |
| 250 | 11250 | 3.12 | 3.93 | 4.59 | 8.43 | 38.25 | 44.4 |
| 300 | 13500 | 3.01 | 3.93 | 5.83 | 10.98 | 52.00 | 57.8 |
| 350 | 15750 | 2.89 | 4.08 | 7.12 | 23.42 | 74.88 | 98.7 |
| 400 | 18000 | 2.98 | 8.42 | 20.77 | 84.40 | 851.42 | 920.7 |

`*` pooled n < 10,000.


### Run `node-open-knee`

commit 6029c755, v24.22.0, AMD Ryzen 7 5800H with Radeon Graphics (16 logical CPUs), build variant node; server pinned to CPUs 0-3, mock upstream 4-5 (delay 0ms), generator unpinned (shares the machine with whatever else is running); server log level INFO (default); 3 repeats x 8s measured after 2s warm-up.

#### open loop (constant arrival rate, latency measured from the intended send time)

| scenario | target req/s | achieved req/s | err % | dropped | p50 ms | p90 ms | p95 ms | p99 ms | p99.9 ms | max ms | n/run | generator send lag p99 ms | srv CPU cores |
|--|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| chat-ns-plain | 450 | 450.0 ±0.0 | 0.000 | 0 | 3.48 ±0.97 | 9.70 ±3.09 | 20.97 ±10.67 | 50.26 ±26.82 | 228.01 ±93.73* | 382.3 | 6750 | 1.67 | 1.03 |
| chat-ns-plain | 500 | 500.0 ±0.0 | 0.000 | 0 | 4.36 ±0.17 | 11.59 ±1.18 | 21.87 ±2.10 | 41.63 ±4.53 | 213.47 ±18.15* | 269.6 | 7500 | 1.83 | 1.04 |
| chat-ns-plain | 550 | 550.0 ±0.0 | 0.000 | 0 | 10.07 ±1.05 | 40.86 ±2.35 | 49.74 ±3.73 | 70.00 ±6.15 | 406.20 ±41.64* | 502.0 | 8250 | 1.61 | 1.09 |
| chat-ns-plain | 600 | 600.0 ±0.0 | 0.000 | 0 | 131.82 ±22.05 | 181.24 ±41.07 | 193.81 ±44.68 | 391.47 ±219.16 | 2503.13 ±959.86* | 3758.1 | 9000 | 1.44 | 1.13 |

Open loop, exact percentiles over the pooled samples of all repeats (latency from the intended send time):

| target req/s | pooled n | p50 | p90 | p95 | p99 | p99.9 | max |
|--:|--:|--:|--:|--:|--:|--:|--:|
| 450 | 20250 | 3.09 | 9.99 | 19.09 | 61.83 | 215.62 | 382.3 |
| 500 | 22500 | 4.34 | 11.40 | 22.43 | 42.24 | 210.37 | 269.6 |
| 550 | 24750 | 10.09 | 40.29 | 49.79 | 71.55 | 407.30 | 502.0 |
| 600 | 27000 | 130.25 | 180.80 | 217.41 | 314.20 | 2948.18 | 3758.1 |

`*` pooled n < 10,000.


### Run `node-mock-delay-50ms`

commit 6029c755, v24.22.0, AMD Ryzen 7 5800H with Radeon Graphics (16 logical CPUs), build variant node; server pinned to CPUs 0-3, mock upstream 4-5 (delay 50ms), generator unpinned (shares the machine with whatever else is running); server log level INFO (default); 3 repeats x 8s measured after 2s warm-up.

#### chat-ns-plain - closed loop

| c | req/s (mean ±95% CI) | err % | p50 ms | p90 ms | p95 ms | p99 ms | p99.9 ms | max ms | n/run | srv CPU cores | srv RSS peak MB | gen CPU |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 1 | 19 ±0 | 0.000 | 53.75 ±0.17 | 54.48 ±0.16 | 55.33 ±1.13 | 57.15 ±2.74 | 57.83 ±2.95* | 61.2 | 148 | 0.09 | 391 | 0.01 |
| 4 | 74 ±0 | 0.000 | 53.84 ±0.05 | 55.82 ±0.43 | 56.87 ±0.29 | 58.62 ±0.49 | 60.45 ±0.35* | 60.8 | 591 | 0.27 | 412 | 0.02 |
| 8 | 148 ±2 | 0.000 | 53.72 ±0.15 | 56.29 ±1.06 | 57.21 ±1.26 | 61.32 ±2.68 | 64.71 ±3.96* | 70.9 | 1182 | 0.46 | 524 | 0.04 |
| 16 | 290 ±10 | 0.000 | 54.31 ±0.49 | 58.61 ±1.84 | 60.01 ±1.83 | 63.70 ±3.32 | 72.03 ±11.02* | 85.2 | 2323 | 0.82 | 560 | 0.07 |
| 32 | 494 ±24 | 0.000 | 64.01 ±1.17 | 69.76 ±1.86 | 72.97 ±2.80 | 87.81 ±4.67 | 104.00 ±4.72* | 110.0 | 3952 | 1.01 | 609 | 0.07 |
| 64 | 587 ±45 | 0.000 | 106.31 ±0.60 | 117.05 ±5.69 | 123.59 ±9.34 | 205.92 ±76.72 | 218.87 ±90.58* | 325.1 | 4695 | 1.11 | 651 | 0.06 |
| 128 | 607 ±9 | 0.000 | 208.14 ±1.37 | 225.79 ±6.98 | 236.92 ±8.13 | 268.89 ±5.55 | 279.43 ±10.87* | 291.8 | 4853 | 1.15 | 668 | 0.07 |
| 512 | 602 ±23 | 0.000 | 882.67 ±32.33 | 933.13 ±57.51 | 947.60 ±53.40 | 963.73 ±59.28 | 967.35 ±63.68* | 1041.3 | 4820 | 1.19 | 866 | 0.07 |
| 1024 | 604 ±56 | 0.000 | 1802.13 ±21.52 | 1871.17 ±27.68 | 1891.34 ±41.20 | 1933.48 ±90.26 | 1936.67 ±88.21* | 2035.9 | 4829 | 1.20 | 1286 | 0.07 |

`*` p99.9 rests on fewer than 10,000 samples per run (the top 0.1% is a handful of requests); treat it as indicative. `max` is the largest value over all repeats.

Exact percentiles over the pooled samples of all repeats of each cell (nearest rank on the full sorted set):

| c | pooled n | mean ms | stddev ms | p50 | p90 | p95 | p99 | p99.9 | max | cold-connect burst: all c sockets answered after (ms) | slowest first response (ms) |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 1 | 444 | 53.94 | 0.83 | 53.76 | 54.39 | 54.88 | 57.94 | 61.24* | 61.2 | 54 | 55 |
| 4 | 1774 | 54.15 | 1.24 | 53.84 | 55.85 | 56.94 | 58.59 | 60.49* | 60.8 | 59 | 59 |
| 8 | 3545 | 54.21 | 1.74 | 53.72 | 56.21 | 57.37 | 61.01 | 68.25* | 70.9 | 68 | 68 |
| 16 | 6968 | 55.10 | 2.82 | 54.15 | 58.61 | 60.47 | 64.50 | 80.71* | 85.2 | 87 | 98 |
| 32 | 11855 | 64.81 | 5.48 | 63.95 | 70.05 | 73.22 | 88.24 | 107.98 | 110.0 | 106 | 116 |
| 64 | 14084 | 108.97 | 17.64 | 106.35 | 116.43 | 126.61 | 175.48 | 316.58 | 325.1 | 150 | 156 |
| 128 | 14559 | 211.35 | 13.91 | 207.99 | 227.25 | 237.89 | 270.10 | 287.71 | 291.8 | 256 | 297 |
| 512 | 14459 | 871.28 | 68.41 | 865.40 | 977.05 | 995.13 | 1018.66 | 1038.69 | 1041.3 | 924 | 1021 |
| 1024 | 14488 | 1805.11 | 62.97 | 1800.84 | 1869.38 | 1895.18 | 2034.14 | 2035.72 | 2035.9 | 1817 | 1890 |

`*` pooled n < 10,000: p99.9 rests on fewer than 10 samples.

| c | lambda req/s | W mean s | L = lambda*W | L sampled in flight | L error % | c (closed-loop population) |
|--:|--:|--:|--:|--:|--:|--:|
| 1 | 18.5 | 0.05394 | 1.00 | 1.00 | -0.21 | 1 |
| 4 | 73.9 | 0.05415 | 4.00 | 4.00 | 0.06 | 4 |
| 8 | 147.7 | 0.05421 | 8.01 | 8.00 | 0.09 | 8 |
| 16 | 290.3 | 0.05511 | 16.00 | 16.00 | -0.00 | 16 |
| 32 | 494.0 | 0.06482 | 32.02 | 32.00 | 0.06 | 32 |
| 64 | 586.8 | 0.10903 | 63.98 | 64.00 | -0.02 | 64 |
| 128 | 606.6 | 0.21136 | 128.22 | 128.00 | 0.17 | 128 |
| 512 | 602.5 | 0.87127 | 524.90 | 512.00 | 2.52 | 512 |
| 1024 | 603.7 | 1.80543 | 1089.87 | 1024.00 | 6.43 | 1024 |

- peak throughput lambda_max = **607 req/s** (mean over repeats)
- unloaded mean latency W(c=1) = 53.940 ms, so the saturation population N* = lambda_max x W(1) = **32.7** concurrent requests
- smallest tested concurrency reaching 95% of lambda_max: **c = 64**
- highest tested concurrency with zero errors in every repeat: **c = 1024**
- run-to-run variance of throughput (coefficient of variation): c=1: 0.0%, c=4: 0.1%, c=8: 0.6%, c=16: 1.3%, c=32: 2.0%, c=64: 3.1%, c=128: 0.6%, c=512: 1.6%, c=1024: 3.8%


### Run `node-log-error`

commit 6029c755, v24.22.0, AMD Ryzen 7 5800H with Radeon Graphics (16 logical CPUs), build variant node; server pinned to CPUs 0-3, mock upstream 4-5 (delay 0ms), generator unpinned (shares the machine with whatever else is running); server log level ERROR; 3 repeats x 8s measured after 2s warm-up.

#### health - closed loop

| c | req/s (mean ±95% CI) | err % | p50 ms | p90 ms | p95 ms | p99 ms | p99.9 ms | max ms | n/run | srv CPU cores | srv RSS peak MB | gen CPU |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 1 | 1751 ±205 | 0.000 | 0.52 ±0.02 | 0.59 ±0.04 | 0.65 ±0.07 | 0.96 ±0.12 | 6.17 ±0.21 | 23.4 | 14008 | 1.12 | 531 | 0.16 |
| 8 | 1926 ±56 | 0.000 | 3.42 ±0.08 | 6.71 ±0.05 | 7.33 ±0.15 | 10.03 ±0.19 | 18.46 ±0.73 | 34.5 | 15406 | 1.17 | 530 | 0.15 |
| 32 | 1932 ±26 | 0.000 | 15.38 ±0.02 | 21.57 ±0.06 | 22.23 ±0.09 | 28.46 ±2.84 | 35.50 ±1.21 | 40.8 | 15454 | 1.16 | 542 | 0.16 |
| 128 | 1933 ±68 | 0.000 | 67.10 ±0.36 | 70.98 ±2.51 | 73.29 ±2.73 | 87.73 ±0.71 | 100.54 ±1.50 | 127.4 | 15461 | 1.16 | 546 | 0.16 |

`*` p99.9 rests on fewer than 10,000 samples per run (the top 0.1% is a handful of requests); treat it as indicative. `max` is the largest value over all repeats.

Exact percentiles over the pooled samples of all repeats of each cell (nearest rank on the full sorted set):

| c | pooled n | mean ms | stddev ms | p50 | p90 | p95 | p99 | p99.9 | max | cold-connect burst: all c sockets answered after (ms) | slowest first response (ms) |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 1 | 42024 | 0.57 | 0.47 | 0.52 | 0.60 | 0.65 | 0.97 | 6.20 | 23.4 | 2 | 2 |
| 8 | 46217 | 4.15 | 1.72 | 3.41 | 6.71 | 7.37 | 10.08 | 18.10 | 34.5 | 5 | 5 |
| 32 | 46361 | 16.57 | 2.90 | 15.38 | 21.55 | 22.26 | 29.84 | 36.12 | 40.8 | 19 | 19 |
| 128 | 46383 | 66.20 | 5.34 | 67.07 | 70.64 | 75.11 | 88.07 | 100.55 | 127.4 | 84 | 97 |

`*` pooled n < 10,000: p99.9 rests on fewer than 10 samples.

| c | lambda req/s | W mean s | L = lambda*W | L sampled in flight | L error % | c (closed-loop population) |
|--:|--:|--:|--:|--:|--:|--:|
| 1 | 1751.0 | 0.00057 | 1.00 | 1.00 | 0.04 | 1 |
| 8 | 1925.7 | 0.00415 | 8.00 | 8.00 | -0.00 | 8 |
| 32 | 1931.7 | 0.01657 | 32.01 | 32.00 | 0.02 | 32 |
| 128 | 1932.6 | 0.06620 | 127.95 | 128.00 | -0.04 | 128 |

- peak throughput lambda_max = **1933 req/s** (mean over repeats)
- unloaded mean latency W(c=1) = 0.571 ms, so the saturation population N* = lambda_max x W(1) = **1.1** concurrent requests
- smallest tested concurrency reaching 95% of lambda_max: **c = 8**
- highest tested concurrency with zero errors in every repeat: **c = 128**
- run-to-run variance of throughput (coefficient of variation): c=1: 4.7%, c=8: 1.2%, c=32: 0.5%, c=128: 1.4%

#### chat-ns-plain - closed loop

| c | req/s (mean ±95% CI) | err % | p50 ms | p90 ms | p95 ms | p99 ms | p99.9 ms | max ms | n/run | srv CPU cores | srv RSS peak MB | gen CPU |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 1 | 452 ±110 | 0.000 | 2.10 ±0.21 | 2.54 ±0.22 | 2.78 ±0.26 | 6.17 ±0.41 | 7.57 ±0.81* | 26.7 | 3619 | 1.09 | 623 | 0.08 |
| 8 | 570 ±41 | 0.000 | 13.35 ±0.32 | 17.37 ±0.38 | 18.34 ±0.49 | 21.63 ±1.95 | 48.93 ±9.63* | 103.1 | 4559 | 1.12 | 623 | 0.07 |
| 32 | 607 ±18 | 0.000 | 53.34 ±0.32 | 63.07 ±0.97 | 65.36 ±0.89 | 79.04 ±7.17 | 115.20 ±11.14* | 128.4 | 4858 | 1.12 | 661 | 0.06 |
| 128 | 633 ±22 | 0.000 | 189.01 ±2.96 | 248.60 ±3.98 | 256.24 ±3.25 | 289.89 ±8.73 | 307.55 ±1.84* | 314.4 | 5065 | 1.15 | 692 | 0.08 |

`*` p99.9 rests on fewer than 10,000 samples per run (the top 0.1% is a handful of requests); treat it as indicative. `max` is the largest value over all repeats.

Exact percentiles over the pooled samples of all repeats of each cell (nearest rank on the full sorted set):

| c | pooled n | mean ms | stddev ms | p50 | p90 | p95 | p99 | p99.9 | max | cold-connect burst: all c sockets answered after (ms) | slowest first response (ms) |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 1 | 10856 | 2.21 | 0.72 | 2.10 | 2.58 | 2.81 | 6.04 | 7.96 | 26.7 | 3 | 4 |
| 8 | 13676 | 14.04 | 2.98 | 13.34 | 17.33 | 18.29 | 21.69 | 54.95 | 103.1 | 15 | 18 |
| 32 | 14573 | 52.76 | 9.23 | 53.29 | 63.19 | 65.38 | 78.22 | 108.82 | 128.4 | 60 | 74 |
| 128 | 15195 | 202.31 | 29.63 | 188.31 | 247.92 | 256.98 | 290.20 | 309.22 | 314.4 | 222 | 256 |

`*` pooled n < 10,000: p99.9 rests on fewer than 10 samples.

| c | lambda req/s | W mean s | L = lambda*W | L sampled in flight | L error % | c (closed-loop population) |
|--:|--:|--:|--:|--:|--:|--:|
| 1 | 452.3 | 0.00222 | 1.01 | 1.00 | 0.56 | 1 |
| 8 | 569.8 | 0.01405 | 8.00 | 8.00 | 0.05 | 8 |
| 32 | 607.2 | 0.05276 | 32.04 | 32.00 | 0.11 | 32 |
| 128 | 633.1 | 0.20234 | 128.10 | 128.00 | 0.08 | 128 |

- peak throughput lambda_max = **633 req/s** (mean over repeats)
- unloaded mean latency W(c=1) = 2.223 ms, so the saturation population N* = lambda_max x W(1) = **1.4** concurrent requests
- smallest tested concurrency reaching 95% of lambda_max: **c = 32**
- highest tested concurrency with zero errors in every repeat: **c = 128**
- run-to-run variance of throughput (coefficient of variation): c=1: 9.8%, c=8: 2.9%, c=32: 1.2%, c=128: 1.4%


### Run `ab-after`

commit 6029c755, v24.22.0, AMD Ryzen 7 5800H with Radeon Graphics (16 logical CPUs), build variant node; server pinned to CPUs 0-3, mock upstream 4-5 (delay 0ms), generator unpinned (shares the machine with whatever else is running); server log level INFO (default); 4 repeats x 8s measured after 2s warm-up.

#### chat-ns-plain - closed loop

| c | req/s (mean ±95% CI) | err % | p50 ms | p90 ms | p95 ms | p99 ms | p99.9 ms | max ms | n/run | srv CPU cores | srv RSS peak MB | gen CPU |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 1 | 445 ±44 | 0.000 | 2.12 ±0.15 | 2.65 ±0.17 | 2.91 ±0.18 | 6.27 ±0.24 | 7.76 ±0.41* | 29.9 | 3558 | 1.07 | 591 | 0.08 |
| 8 | 486 ±27 | 0.000 | 15.80 ±0.59 | 19.79 ±0.69 | 21.33 ±0.77 | 24.99 ±1.53 | 42.85 ±3.16* | 65.8 | 3890 | 1.10 | 601 | 0.07 |
| 32 | 519 ±9 | 0.000 | 60.14 ±1.23 | 75.18 ±1.22 | 78.70 ±1.23 | 90.41 ±3.29 | 122.34 ±6.05* | 130.2 | 4149 | 1.11 | 643 | 0.06 |

`*` p99.9 rests on fewer than 10,000 samples per run (the top 0.1% is a handful of requests); treat it as indicative. `max` is the largest value over all repeats.

Exact percentiles over the pooled samples of all repeats of each cell (nearest rank on the full sorted set):

| c | pooled n | mean ms | stddev ms | p50 | p90 | p95 | p99 | p99.9 | max | cold-connect burst: all c sockets answered after (ms) | slowest first response (ms) |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 1 | 14233 | 2.25 | 0.77 | 2.11 | 2.68 | 2.93 | 6.20 | 7.75 | 29.9 | 4 | 4 |
| 8 | 15561 | 16.44 | 2.94 | 15.85 | 19.74 | 21.43 | 25.65 | 43.88 | 65.8 | 22 | 34 |
| 32 | 16597 | 61.69 | 10.95 | 60.12 | 75.24 | 78.90 | 91.96 | 119.45 | 130.2 | 65 | 71 |

`*` pooled n < 10,000: p99.9 rests on fewer than 10 samples.

| c | lambda req/s | W mean s | L = lambda*W | L sampled in flight | L error % | c (closed-loop population) |
|--:|--:|--:|--:|--:|--:|--:|
| 1 | 444.8 | 0.00225 | 1.00 | 1.00 | 0.22 | 1 |
| 8 | 486.3 | 0.01646 | 8.00 | 8.00 | 0.04 | 8 |
| 32 | 518.7 | 0.06169 | 32.00 | 32.00 | -0.01 | 32 |

- peak throughput lambda_max = **519 req/s** (mean over repeats)
- unloaded mean latency W(c=1) = 2.253 ms, so the saturation population N* = lambda_max x W(1) = **1.2** concurrent requests
- smallest tested concurrency reaching 95% of lambda_max: **c = 32**
- highest tested concurrency with zero errors in every repeat: **c = 32**
- run-to-run variance of throughput (coefficient of variation): c=1: 6.2%, c=8: 3.5%, c=32: 1.1%

#### chat-stream-plain - closed loop

| c | req/s (mean ±95% CI) | err % | p50 ms | p90 ms | p95 ms | p99 ms | p99.9 ms | max ms | n/run | srv CPU cores | srv RSS peak MB | gen CPU |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 1 | 350 ±55 | 0.000 | 2.73 ±0.27 | 3.30 ±0.23 | 3.65 ±0.29 | 6.56 ±0.57 | 8.36 ±0.40* | 30.8 | 2802 | 1.08 | 593 | 0.08 |
| 8 | 468 ±34 | 0.000 | 15.85 ±0.26 | 20.16 ±0.70 | 23.21 ±3.16 | 35.63 ±11.49 | 52.89 ±4.50* | 58.3 | 3746 | 1.11 | 606 | 0.09 |
| 32 | 518 ±21 | 0.000 | 62.89 ±1.47 | 71.24 ±2.68 | 74.23 ±2.71 | 90.82 ±3.59 | 111.31 ±4.92* | 124.4 | 4140 | 1.11 | 644 | 0.08 |

`*` p99.9 rests on fewer than 10,000 samples per run (the top 0.1% is a handful of requests); treat it as indicative. `max` is the largest value over all repeats.

Exact percentiles over the pooled samples of all repeats of each cell (nearest rank on the full sorted set):

| c | pooled n | mean ms | stddev ms | p50 | p90 | p95 | p99 | p99.9 | max | cold-connect burst: all c sockets answered after (ms) | slowest first response (ms) |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 1 | 11206 | 2.85 | 0.83 | 2.75 | 3.34 | 3.70 | 6.55 | 8.43 | 30.8 | 4 | 4 |
| 8 | 14984 | 17.09 | 4.37 | 15.82 | 20.09 | 21.89 | 41.53 | 55.03 | 58.3 | 16 | 17 |
| 32 | 16561 | 61.85 | 9.32 | 62.48 | 71.85 | 74.95 | 89.29 | 113.00 | 124.4 | 63 | 66 |

`*` pooled n < 10,000: p99.9 rests on fewer than 10 samples.

| c | lambda req/s | W mean s | L = lambda*W | L sampled in flight | L error % | c (closed-loop population) |
|--:|--:|--:|--:|--:|--:|--:|
| 1 | 350.2 | 0.00287 | 1.01 | 1.00 | 0.63 | 1 |
| 8 | 468.3 | 0.01711 | 8.01 | 8.00 | 0.17 | 8 |
| 32 | 517.5 | 0.06188 | 32.03 | 32.00 | 0.08 | 32 |

- peak throughput lambda_max = **518 req/s** (mean over repeats)
- unloaded mean latency W(c=1) = 2.874 ms, so the saturation population N* = lambda_max x W(1) = **1.5** concurrent requests
- smallest tested concurrency reaching 95% of lambda_max: **c = 32**
- highest tested concurrency with zero errors in every repeat: **c = 32**
- run-to-run variance of throughput (coefficient of variation): c=1: 9.9%, c=8: 4.6%, c=32: 2.6%

## 9. Follow-up: `GET /v1/models` (6 ms -> 0.7 ms CPU per call)

Profiling `/v1/models` on the copied database (`--cpu-prof`, c=8) showed the catalogue projection was already skipped
by the hash gate, but the gate itself cost almost everything: `modelsInputsEtag` ran `JSON.stringify` + SHA-256 over
5,291 custom models and 1,085 aliases on every request (about 55% of server CPU), and `getCustomModels()` rebuilt a
5,291-element array with `Object.values` on every call (about 17%).

Fix: the alias map and custom-model list are digested once per distinct object (the repos hand back the same object until a
write or the 5 s kv cache TTL replaces it, so an edit still moves the ETag on the very next request), and `getCustomModels()`
derives its (frozen) array once per cached object. The response bytes, ETag semantics, per-key restricted-catalogue
narrowing and fail-closed behaviour are unchanged.

| Metric (`models`, one process, Node build, 3 repeats) | Before | After |
|---|--:|--:|
| c=8 throughput (profiling run) | 178 - 183 req/s | 1,130 req/s |
| c=32 throughput | 172 - 178 req/s | 1,393 req/s |
| c=128 throughput | 172 req/s | 1,336 - 1,388 req/s |
| c=1 latency p50 / p99 | n/a | 0.72 - 0.81 ms / 1.3 - 1.6 ms |
| CPU per call | ~6 ms | ~0.7 ms |

Raw runs: `runs/models-fix/`. `/v1/models` is now about 8x faster, the same order as a chat completion, and no longer
the slowest endpoint. Cold-isolate Cloudflare reads of the custom-model table (section 6) are unchanged.

