# Cloudflare "extreme" persistence profile

Why: on Workers, every proxied request used to cost metered storage operations
even though the router keeps its working set in isolate memory. Two stores are
billed per row, and both are SQLite:

* **D1** (`ninerouter_rows`, the durable copy of the app database);
* the **`GatewayCoordinator` Durable Object** (`storage = "sqlite"` in `wrangler.toml`),
  which holds model locks, adaptive scores, rate-limit/lease counters, the
  control version and cluster metrics. Its `storage.get/put/delete` are billed
  as rows read / written, and each RPC is a billed request.

`NINEROUTER_D1_PROFILE` selects the behaviour:

| value | meaning |
|---|---|
| `extreme` | **Default on the Cloudflare runtime.** Everything below. |
| `standard` | The previous per-request behaviour (kept for rollback and as the "before" arm of the tests). Default outside Cloudflare. |

Set it as a Worker var/secret (`NINEROUTER_D1_PROFILE = "standard"`) to roll back
without a code change. The profile is read on every call, so tests can flip it.

## What the extreme profile changes

Request path (warm isolate):

1. **Outcome and metric reports are coalesced.** `recordProviderOutcome` and
   `recordRequestOutcome` used to be two Durable Object RPCs per request
   (`recordOutcome`: 1 read + 1 write, `recordMetrics`: 3 reads + 3 writes).
   They are aggregated in isolate memory (`src/lib/coordinator/coalescer.js`)
   and sent as one `recordOutcomes` call per provider and one
   `recordMetricsBatch` call per flush, every `NINEROUTER_COORDINATOR_FLUSH_MS`
   (30 s) or `NINEROUTER_COORDINATOR_FLUSH_MAX` (500) events. The Durable Object
   ends up in the same state as with per-request calls (counts, trailing failure
   run, cost, last status, totals and tenant rollups are identical; the EWMA
   latency is folded with the batch mean, exact for constant latency). Pinned by
   `tests/unit/coordinator-extreme-profile.test.js` against the real
   `GatewayCoordinator` class.
2. **`getActiveLocks` and adaptive `getScores` reads are cached** for
   `NINEROUTER_COORDINATOR_READ_TTL_MS` (5 s). The isolate that sets or clears a
   lock invalidates its own cache immediately.
3. **`clearLock` is no longer called for healthy connections.** A selected
   connection cannot hold an active coordinator lock (locked ones are filtered
   out), and the call ran on *every* success whenever a connection had
   `testStatus: "active"`. Error state persisted by an older build is healed with
   one `providerConnections` write on the next success, not one RPC per request
   forever.
4. **Operational state is memory-only.** Model locks and proxy-pool quarantine
   no longer rewrite the connection / settings row on every failure
   (`isMemoryOnlyState()` in `usageRepo.js`). Locks are still published to the
   coordinator, so other isolates honour them.
5. **Unknown API keys cannot drive D1 reads.** A lookup of a key the isolate does
   not know used to force a re-read of the whole `apiKeys` table (10 rows here,
   two statements) per attempt, so a scanner sending random keys paid D1 for
   every request. The forced refresh is skipped when the table was read within
   `NINEROUTER_UNKNOWN_KEY_REFRESH_MS` (30 s). A key created or rotated on
   another isolate still arrives through the control-version poll, so unknown
   keys stay rejected (fail closed) and legitimate new keys appear promptly.
6. **Control-version polls are batched and slower.** One `getVersions` RPC refreshes
   every known domain (was one `getVersion` per domain), every
   `NINEROUTER_CONTROL_POLL_MS` (10 s, was 2 s).

Cold isolate:

7. **`kv` hydrates per scope** (`ensureKvScope`) instead of the whole table. On the
   production-shaped dataset (42 connections, 31 pools, 8 combos, 5,291 custom
   models) a cold chat request read 5,425 rows, 5,291 of them custom models it
   never used. It now reads 93. `/v1/models` legitimately needs
   `customModels` and still reads them.
8. **`usageDaily` is point-loaded.** The analytics flush loads only the day it
   merges into (`ensureRow`); cold isolates no longer read 30+ history rows.
   `saveRequestUsage`'s buffered path no longer pulls `usageHistory`/`usageDaily`
   at all.
9. **No DDL on boot.** `CREATE TABLE IF NOT EXISTS` ran on every cold isolate; the
   table is now created only when the first read reports it missing.
10. **No duplicate boot reads.** `settings` and `apiKeys` are not re-read right
    after the adapter has just loaded them (`freshWithin`).

Correctness fixes found on the way (they were wrong before, independent of cost):

* repos that read `kv` directly (`disabledModels`, `pricing` writes,
  `addCustomModel`, `deleteProviderNode`) and `exportDb`/`importDb` now make the rows
  resident first; before, a cold isolate saw no disabled models, merged pricing into
  an empty row, and exported/imported without its connections and kv scopes.

## Durability: what stays write-through

Unchanged and still durable: provider credentials and token refresh
(`providerConnections`), settings, API keys, combos, the budget debit ledger
(`forcePersist` usage rows), the explicit `importDb`. The per-key enforcement
(`validateAndEnforceApiKey`, `modelAcl`, force pins, rate limit, concurrency
lease, budget reservation) goes through the same code and the same fail-closed
coordinator calls as before; a key with `rateLimitRequests` still costs one
`checkRateLimit` RPC (1 row read + 1 written) per request and `maxConcurrentRequests`
two (begin/end), because those counters must be exact across isolates.

## Analytics fidelity trade-offs (explicit)

| What | Before | Extreme |
|---|---|---|
| Usage Keys analytics (`usageDaily` aggregates incl. `byApiKey`, lifetime counter) | buffered in memory, flushed every 100 requests / 10 min (2 D1 rows per flush) | **unchanged** (same buffer and thresholds). Isolate eviction still loses the unflushed tail. |
| Per-request `usageHistory` rows | skipped on Cloudflare (low-write) | unchanged |
| Cluster metrics / alerts (`getMetrics`) | live | lag by up to 30 s (or 500 events); an evicted isolate loses its unflushed tail |
| Adaptive routing scores | live | same lag; this isolate additionally sees its own outcomes only after the flush |
| A lock set by another isolate | seen on the next request | seen after at most 5 s |
| API key revoked / kill switch flipped on another isolate | seen within 2 s | seen within 10 s (the writing isolate is immediate) |
| Locks and quarantine | persisted on the connection / settings row | isolate memory + coordinator lock; lost on eviction (costs one retry, absorbed by fallback) |

Explicit `lowWriteMode` settings still win: `false` restores per-request analytics rows,
`"extreme"` additionally drops the daily aggregates and lifetime counter (the dashboard
usage page and token-limit enforcement then degrade).

Known pre-existing limits this work did not change (found while measuring):

* `tokenLimitDaily` / `tokenLimitMonthly` sum `usageHistory`, which low-write mode on
  Cloudflare does not populate, so those limits only see budget-ledger rows there.
* ~~Budget reads load the whole `usageHistory` table on first use per isolate.~~ Fixed: a budget read now
  loads only that key's rows inside its budget window (two lazy partial indexes on `ninerouter_rows` keep the
  select index-driven), and ledger inserts take ids from a clock+random base instead of continuing a table
  that is no longer fully resident. Cold budget read in the test dataset (4,031-row history): 4,082 to 81 D1
  rows read. The dashboard usage views still load the whole table on demand. The table itself still grows
  with every debit; nothing prunes it.
* A budget commit (`forcePersist`) used to be skipped by low-write mode, which is the default on Cloudflare,
  so a committed debit never reached the spend the budget reads. It is now always written.
* The cron `*/10` tick calls the Worker through the `WORKER_SELF_REFERENCE` service binding (no public
  hostname). It is one extra invocation and lands on an arbitrary isolate, so it can only flush that
  isolate's buffer.
* After the first forced `resync(..., ["settings"])`, the unscoped 5-minute resync that
  would refresh `providerConnections`/`kv` never fires on a busy isolate, because scoped
  forced resyncs also move its timestamp. A connection edited on another isolate is
  picked up when this isolate is recycled. Left as is: "fixing" it would add reads.

## Measured operations per request

Produced by `tests/unit/cloudflare-d1-ops.test.js` (real `handleChat` pipeline, counting
fake D1 over node:sqlite, the real `GatewayCoordinator` over a counting storage, local
mock upstream; dataset 42 connections, 8 API keys, 8 combos, 31 pools, 5,291 custom models,
30 usage days). Raw numbers: `docs/benchmarks/d1-ops-before.json` (code at `19953777`) and
`d1-ops-after.json`. Each cell is amortised per request: the request path plus its share
of the batched flush (the harness flushes once per measured window, i.e. every 20-30
requests; production flushes every 100 requests / 10 min for D1 and 500 events / 30 s for
the coordinator, which is 5-25x less).

Cell format: **D1 rows read / rows written / statements · DO RPCs / DO rows read / DO rows written**.

| Scenario (per request) | Before: D1 rows read / written / queries · DO rpcs / rows read / rows written | After (extreme) |
|---|---|---|
| cold: chat non-stream | 5425 / 0 / 7 · 5 / 9 / 4 | 93 / 0 / 4 · 3 / 6 / 0 |
| cold: chat stream | 5425 / 0 / 7 · 5 / 9 / 4 | 93 / 0 / 4 · 3 / 6 / 0 |
| cold: chat budget key | 5425 / 0 / 7 · 7 / 11 / 4 | 93 / 0 / 5 · 5 / 8 / 0 |
| cold: GET /v1/models | 5425 / 0 / 6 · 2 / 2 / 0 | 5384 / 0 / 6 · 2 / 3 / 0 |
| cold: GET /api/health | 0 / 0 / 0 · 0 / 0 / 0 | 0 / 0 / 0 · 0 / 0 / 0 |
| cold: POST /api/cron/flush | 0 / 0 / 0 · 0 / 0 / 0 | 0 / 0 / 0 · 0 / 0 / 0 |
| warm: chat non-stream x20 | 0 / 0.100 / 0.100 · 3 / 7 / 4 | 0.050 / 0.100 / 0.150 · 0.050 / 0.050 / 0.050 |
| warm: chat stream x20 | 0 / 0.100 / 0.100 · 3 / 7 / 4 | 0 / 0.100 / 0.100 · 0.050 / 0.050 / 0.050 |
| warm: chat pinned key (force pins + ACL) x20 | 0.500 / 0.100 / 0.150 · 3 / 7 / 4 | 0 / 0.100 / 0.100 · 0.050 / 0.050 / 0.050 |
| warm: chat rate-limited key x20 | 0.500 / 0.100 / 0.150 · 4 / 8 / 5 | 0 / 0.100 / 0.100 · 1.050 / 1.050 / 1.050 |
| warm: chat max-concurrent key x20 | 0.500 / 0.100 / 0.150 · 5 / 9 / 6 | 0 / 0.100 / 0.100 · 2.050 / 2.050 / 2.050 |
| warm: chat token-limit key x20 | 0 / 0.100 / 0.100 · 3 / 7 / 4 | 0 / 0.100 / 0.100 · 0.050 / 0.050 / 0.050 |
| warm: chat budget key x20 | 0 / 0.100 / 0.100 · 5 / 9 / 4 | 0 / 0.100 / 0.100 · 2.050 / 2.050 / 0.050 |
| warm: GET /v1/models x20 | 0 / 0 / 0 · 0 / 0 / 0 | 0 / 0 / 0 · 0 / 0 / 0 |
| warm: GET /api/health x20 | 0 / 0 / 0 · 0 / 0 / 0 | 0 / 0 / 0 · 0 / 0 / 0 |
| warm: invalid API key x20 | 20 / 0 / 2 · 0 / 0 / 0 | 0 / 0 / 0 · 0 / 0 / 0 |
| warm: upstream 500 x5 | 0 / 0.200 / 0.200 · 1.800 / 4.600 / 1.200 | 0 / 0 / 0 · 0.800 / 1.600 / 0.600 |
| steady 1h @ 0.1 req/s (360 req) | 0.367 / 0.039 / 0.106 · 6 / 10 / 5 | 0.369 / 0.039 / 0.108 · 2.336 / 5.342 / 0.342 |
| steady 1h @ 1 req/min (60 req) | 2.200 / 0.233 / 0.633 · 6 / 10 / 5 | 2.217 / 0.233 / 0.650 · 3.017 / 6.050 / 1.050 |
| warm: observability on x20 | 0 / 0.100 / 0.100 · 4 / 8 / 5 | 0.050 / 0.100 / 0.150 · 0.050 / 0.050 / 0.050 |

"Rows written" counts logical rows; D1 additionally bills index maintenance (a new
`ninerouter_rows` row also writes the primary-key index), so D1 writes can be up to 2x for
inserts (`rowsWrittenWorstCase` in the JSON). The counting D1 is a model, not Cloudflare's
meter: nothing was compared with a real `meta.rows_read`.

## Knobs

| env | default (extreme) | effect |
|---|---|---|
| `NINEROUTER_D1_PROFILE` | `extreme` on Cloudflare | `standard` restores per-request behaviour |
| `NINEROUTER_COORDINATOR_READ_TTL_MS` | 5000 | lock / score cache TTL (0 disables) |
| `NINEROUTER_COORDINATOR_FLUSH_MS` | 30000 | max age of a pending outcome batch |
| `NINEROUTER_COORDINATOR_FLUSH_MAX` | 500 | events that trigger an early flush |
| `NINEROUTER_CONTROL_POLL_MS` | 10000 | control-version poll period |
| `NINEROUTER_UNKNOWN_KEY_REFRESH_MS` | 30000 | min gap between forced apiKeys refreshes |

Size and memory limits (request bodies, upstream bodies, stream capture, log level, outbound concurrency)
are listed in `docs/CLOUDFLARE_COST_CHECKLIST.md`.
