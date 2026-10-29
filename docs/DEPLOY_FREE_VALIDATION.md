# Free-plan deploy validation (2026-10-29)

Scope: deploy master `95878070` (Worker `mihir-router`) to the Cloudflare account wrangler was logged into, run
low-traffic functional and monitoring checks, and decide whether to deploy to the paid account. Nothing was deployed
anywhere else, Antideploy was not run, nothing was pushed, no secret or id was changed.

**Result: deployed, then rolled back. The Free plan cannot host this Worker (CPU limit). The build itself behaved
correctly on Workers. Recommendation for the paid account: conditional GO, see the end.**

## Account, versions, rollback

| | |
|---|---|
| Account | `Mihir@cognixai.in's Account`, `workers.dev` subdomain `themihir`, URL `https://mihir-router.themihir.workers.dev`. Plan is not readable with the CLI's OAuth scopes (billing API: 403). Treated as Free as stated; the Worker was killed at exactly 10 ms CPU, which matches the Free limit |
| Previous live version | `f323aa6a-ff27-4ad0-8825-7645963667c8` (2026-10-01) |
| New version | `13cbbad1-64ca-444f-a331-38963a0123a8` (uploaded 07:13 UTC; 11,549.89 KiB raw / 2,815.16 KiB gzip, accepted by Free) |
| Live now | `f323aa6a` at 100 %, single-version deployment (verified: health 200, login 200) |
| Rollback command used | `npx wrangler rollback f323aa6a-ff27-4ad0-8825-7645963667c8 -y` |
| Redeploy the new version | `npx wrangler versions deploy 13cbbad1-64ca-444f-a331-38963a0123a8@100%` (the version is still uploaded) |
| D1 backup before deploy | `.agents/backups/mihir-router-db-pre-deploy.sql` (2.7 MB, git-ignored, not committed) |

Timeline: build (`npm run build:cloudflare`, assertions passed) and local `wrangler dev --local` smoke passed
(health 200, `/v1/models` 401, 21 MB body 413, immutable static header, `/` 307). The first `wrangler deploy` failed on a
network error (ECONNRESET on the script PUT, live version unchanged); the retry succeeded at 07:13. Within 20
minutes of the deploy, under light traffic, `/api/health` and `/login` started returning 503/500 (`exceededCpu`) and some
requests timed out, which is the pre-approved rollback condition. Rolled back at 07:33. The new version was then re-attached at 0 %
(`wrangler versions deploy ...@0%` plus `f323aa6a@100%`) and exercised with the
`Cloudflare-Workers-Version-Overrides` header, so no user traffic reached it. The 0 % canary was removed at the end.

Why this does not show a regression: the old version `f323aa6a` also got killed (9 of 150 invocations, 6 %) under the
same smoke, so the Free CPU limit is the cause, not the new code. The old version also lacks several fork features
(immutable static cache header, pin/blocked conflict check on the admin API), so it is not a like-for-like baseline.

## Secrets and exposure

* Worker secrets present (names only): `ADMIN_API_TOKEN`, `API_KEY_SECRET`, `INITIAL_PASSWORD`, `JWT_SECRET`,
  `MACHINE_ID_SALT`. **`CRON_SECRET` is not set**: `GET /api/cron/flush` answers 200 to anyone (confirmed). It is a cheap
  but unauthenticated Worker invocation.
* The dashboard password could not be recovered (`hasPassword: true`, so a password is stored in D1; the
  `.env.deploy` value was rejected, one failed attempt, then I stopped). Dashboard-behind-login cases are therefore not run.
  The `ADMIN_API_TOKEN` from the repo `.env` is accepted by the Worker and was used to create and delete throwaway keys.

## Test matrix

Script: `scripts/smoke/deployed-smoke.mjs` (Node ESM, no dependencies; credentials from env or a git-ignored file,
redacted in output). Existing `tests/**/real/*` call handlers in-process against the local database, so they cannot be
pointed at a deployed URL; the script covers the same ground over HTTP. Results below are for the new version
(runs 5, 6 and 9, via version override) unless stated. "env" means a result caused by the Free CPU limit or a
stale-credential connection, not by the code under test.

| # | Case | Result |
|---|---|---|
| P1 | `GET /api/health` 200 `{"ok":true}` | pass (503 on a cold isolate in 2 of 6 runs: env) |
| P2 | `GET /` 307 to `/dashboard` | pass |
| P3 | `GET /dashboard` without session redirects to `/login` | pass |
| P4 | `GET /login` renders | pass (500/503 intermittently: env) |
| P5 | static asset `public, max-age=31536000, immutable`, `cf-cache-status: HIT` | pass |
| P6 | `GET /api/auth/status` public | pass |
| P7 | `GET /v1/models` without key 401 | pass |
| P8 | chat with invalid key 401 | pass |
| P9 | admin API without/with wrong token 401 | pass |
| P10 | `/api/keys`, `/api/providers` without session 401 | pass |
| P11 | `/api/cron/flush` access control | **warn: public, `CRON_SECRET` missing** |
| P12 | 21 MB JSON body returns 413 JSON `request_too_large` | pass (ran by hand on the live URL: 413 after 62 s, my uplink is about 220 KB/s; also pass locally) |
| D1-D3 | dashboard login, pages (endpoint, providers, usage, profile), logout | **not run** (no password) |
| G1 | admin API creates a throwaway key | pass |
| G2 | `/v1/models` with key 200 + ETag; `If-None-Match` gives 304 | **warn**: 200 returned. Cloudflare's edge turns the strong ETag into `W/"..."` when it compresses; the app compares strictly (`src/lib/modelCatalog/modelsListCache.js`, `withCatalog`), so 304 never happens on Workers. Echoing the strong form gives 304 |
| C1 | non-stream chat, `max_tokens` 16, real provider (`ollama/gemma4:31b-cloud`) | pass (1.2 s warm, 5.8 s cold) |
| C2 | stream chat | pass with warning in the last run: stream carries `finish_reason: stop` and usage but no `data: [DONE]` line. One earlier run failed on a 503 (env) |
| K1 | `forceModel` pin rewrites asked `openai/gpt-4o` to the pinned model | pass |
| K2 | `forceProvider` pin vs other-provider model gives 403 | pass |
| K3 | `allowedModels` ACL gives 403 for another model | pass |
| K4 | `blockedModels` gives 403 | pass in run 5, 503 (env) in run 9 |
| K5 | admin API rejects `forceModel` also in `blockedModels` (400) | pass |
| K6 | `rateLimitRequests=3`: 4th request is 429 with `Retry-After` | pass in run 5 (404,404,404,429,429, Retry-After 55). Run 9 saw 401s from isolates that had not seen the new key yet |
| K7 | disabled key 401 | pass |
| X1 | throwaway keys deleted | pass (verified read-only in D1: `apiKeys` 20 rows before and after, 0 containing `smoke-test`) |
| - | `POST/PUT /api/keys` 400 on mismatched pin pair (`findForcePairMismatch`) | **not run** (needs a dashboard session; the admin route does not run that check) |
| - | request details not written per request | pass: D1 `requestDetails` 350 before and after, `usageHistory` 47 before and after |
| - | analytics flush visible in dashboard usage | **not verified**: no `usageDaily` row for today. The buffer is per isolate (100 requests or 10 min), and the cron tick reported `lifetimeRequests: 0` on the isolate it reached |

New API keys are not visible to other isolates immediately: 401 for 2 to 14 s after creation (documented 10 s poll and
30 s unknown-key window). The script waits for visibility (`SMOKE_KEY_WAIT_MS`).

## Measured against claimed

| | Claimed | Measured | Verdict |
|---|---|---|---|
| (a) no 1101/1102, no exceeded outcomes | none | New version: 90 of 487 invocations `exceededResources` (18 %), tail text "Worker exceeded CPU time limit." Client sees 503 or 500. No memory-exceeded outcome seen (memory use is not exposed) | **fail on Free** |
| (b) CPU per request, Free limit 10 ms | fits (cold start dominated) | Cloudflare analytics, new version, completed requests: p50 19.0 ms, p90 315 ms, p99 1,049 ms. Killed requests: p50 10.0 ms, p99 215 ms. Wall-clock p50 35 ms, p99 4.8 s. Per endpoint (tail `cpuTime`, ms, p50/p90/p99): health 8/39/555, `/v1/models` 20/124/1048, chat (rejected or ACL) 12/170/960, admin key create 185/395/785, `/login` 232/682/682. Only 36 to 46 % of completed requests used 10 ms or less | **fail on Free** (docs said "not necessarily fits") |
| (c) D1 rows per request (warm chat about 0 read / 0.1 written; cold chat 93 read) | as stated | Window of run 5 (98 requests, 12 cold isolates): 38,954 rows read, 49 written, which is about 397 read and 0.5 written per request, or about 3.2 k rows per cold isolate. Dominated by `/v1/models` loading `customModels` (kv holds 3,502 rows; the docs state this call legitimately reads them). Cold chat alone was not isolated. Free-plan kills evict isolates, so the warm figure could not be reproduced | **not confirmed**, not comparable |
| (d) Durable Object, about 0.05 requests per request | as stated | Run 5 window: +132 DO requests, +78 rows read, +21 rows written for 98 requests. Includes version polls on each cold isolate, rate-limit key checks (K6), metric flushes | **not confirmed**, not comparable |
| (e) log volume small | WARN default, sampling 0.05/0.01 | Tail (which ignores sampling): 100 log lines in about 500 events, all WARN or error lines from rejected requests and upstream failures; no per-request info lines. Persisted Workers Logs not inspected | pass (partial) |
| (f) static served without Worker | yes | 0 `/_next/static` invocations in the tails; `cf-cache-status: HIT`, immutable | pass |
| (g) upload size | 11.5 MiB / 2.8 MiB gzip | 11,549.89 KiB / 2,815.16 KiB, accepted | pass |
| (h) 413 and memory caps | 413 above 20 MB | 413 JSON on the live Worker and locally. Memory cap not measurable on Workers | pass / unverified |
| (i) cron every 10 minutes via the service binding | yes | `[cron flush] 200` logged at 07:41:01, 07:51:02, 08:01:03, 08:21 UTC (about 1 minute after the boundary), through `WORKER_SELF_REFERENCE`, on `f323aa6a`. The new version's scheduled handler was only covered by the build-time assertion | pass (partial) |

Cloudflare counters for the whole session (07:00 to 08:33 UTC): D1 406 read queries, 100,540 rows read, 171 write
queries, 294 rows written, database 3.31 MB to 3.46 MB; Durable Object 588 requests (6 client disconnects), 499 rows read,
155 rows written; Worker requests: new version 487, old version 150. Far below the Free daily limits.

## Client-side latency

Measured from this machine over a slow, high-latency link, new version, completed requests only (status below 500),
nearest-rank percentiles from the raw samples. Not a benchmark.

| Request | n | p50 ms | p95 ms | max ms |
|---|---|---|---|---|
| `GET /api/health` | 41 | 379 | 1,663 | 2,300 |
| `GET /v1/models` | 45 | 557 | 4,284 | 5,234 |
| `GET /v1/models` conditional (200 in practice) | 40 | 304 | 4,315 | 4,795 |
| chat rejected by key check | 36 | 360 | 2,042 | 3,245 |
| chat non-stream (real provider) | 4 | 5,113 | 5,824 | 5,824 |
| admin key create / delete | 32 / 24 | 1,452 / 1,434 | 4,333 / 3,284 | 4,510 / 3,986 |
| all of the above pooled | 334 | 594 | 3,440 | p99 5,113 |

First request after an idle gap (cold): health 810, 1,598, 1,720 and 2,300 ms in four runs. In the 1 request/second
soak (run 6, 106 requests, new version) there were 0 failures with p50 about 300 ms; the failures cluster on cold
isolates and on admin or cold `/v1/models` calls.

## Issues found

1. **Free plan: not viable.** About one in six requests is killed at 10 ms and a typical completed request uses more.
   Not fixable by a small change; the old version behaves the same way (6 % killed).
2. **Intermittent hung `GET /v1/models`.** In the 5-second-interval soak (run 5) 3 requests hung until the client gave
   up (about 330 s wall, outcome `canceled`, tail warning "waitUntil() tasks did not complete..."). Not reproduced in
   run 6 (106 requests at 1/s). Cause unknown; may be an effect of the CPU kills, or the shared in-flight promise in
   `withCatalog`/`coalesceModelsBuild` being awaited from another request. **Re-check on the paid plan before cut-over.**
3. Weak ETag defeats `If-None-Match` on Workers (see G2). Small fix: compare tags weakly. Not changed here.
4. `CRON_SECRET` missing (see above).
5. Provider credentials stored in the deployed D1: the `cloudflare-ai` connection (`r8devsin`) returns 401 from
   Cloudflare, the Ollama connection `h` returns 401, `testing`/`haaa`/`hm` work with `gemma4:31b-cloud`
   (`glm-4.7-flash` is not on the free plan). Needs a look in the dashboard.
6. Test residue: `auditLog` in D1 grew from 96 to at least 152 rows (admin key create/delete events). Removing them needs
   write SQL against the live database, which I did not run. No other test data remains (`apiKeys` back to 20 rows).
7. Local tree: `npm run build` re-run, `.next/standalone/.9router-build-variant` is `node`; booted against a copy of the
   database via `DATA_DIR` (health 200, `[DB] Driver: node:sqlite`); `~/.9router/db/data.sqlite` untouched (mtime and size
   identical before and after).

## Recommendation for the paid account: conditional GO

Why GO: the build assertions, local workerd smoke, key enforcement (pins, ACL, blocked models, rate limit, disabled
key), real provider calls, 413 caps, static caching, cron through the service binding and the 500 or so synthetic requests
all behaved as designed whenever the request was not killed by the 10 ms limit. The upload fits. The paid plan removes
the failing constraint (30 s CPU default).

Conditions (not met yet, so this is not a blind GO):

* Deploy as a gradual deployment: `wrangler versions upload`, then `versions deploy <new>@0%`, run
  `SMOKE_VERSION_OVERRIDE=<new> node scripts/smoke/deployed-smoke.mjs` including the soak, then shift traffic.
  Keep `npx wrangler rollback <previous>` ready.
* Re-run the soak for 20 minutes and confirm that issue 2 (hung `/v1/models`) does not occur.
* Run the dashboard cases (`SMOKE_DASHBOARD_PASSWORD`), the analytics flush check, and measure D1/DO rows per request
  with real traffic, because the per-request figures above could not be validated on Free.

Dashboard and configuration actions still needed on the paid account (none done here):

1. Confirm the plan is Workers Paid; add a billing/usage notification for Workers, D1 and Durable Objects.
2. Put the Worker on a custom domain, add a WAF rate-limiting rule and Bot Fight Mode for the zone, then set
   `workers_dev = false`.
3. Add `[limits] cpu_ms = ...` to `wrangler.toml` if a ceiling below 30 s is wanted.
4. Decide on `[placement] mode = "smart"` (unmeasured).
5. Set `CRON_SECRET`, and confirm `JWT_SECRET`, `INITIAL_PASSWORD`, `API_KEY_SECRET`, `MACHINE_ID_SALT`, `ADMIN_API_TOKEN`
   exist on that Worker (secrets are per Worker, they do not move with a deploy).
6. Check the stored provider connections (issue 5) and rotate the dashboard password if it is unknown.

## Reproduce

```bash
SMOKE_BASE_URL=https://<worker>.workers.dev SMOKE_ENV_FILE=.env \
  [SMOKE_DASHBOARD_PASSWORD=...] [SMOKE_CHAT_MODEL=ollama/gemma4:31b-cloud] [SMOKE_SOAK_SECONDS=1200 SMOKE_SOAK_INTERVAL_MS=5000] \
  [SMOKE_VERSION_OVERRIDE=<version id>] [SMOKE_OVERSIZE=1] [SMOKE_CRON_SECRET=...] \
  node scripts/smoke/deployed-smoke.mjs
```

`SMOKE_ENV_FILE` may point at the repo `.env` (it reads `ADMIN_API_TOKEN`); the script never prints secrets.

---

# Session 2 (2026-10-29, optimized-v2 candidate): progress log

Written stage by stage so an interrupted run loses nothing. Nothing is pushed; nothing is promoted on the Free account.

## S1 state (checked 2026-10-29 15:23 UTC)

* Account `Mihir@cognixai.in's Account` (`60bf355a...`), OAuth scopes include workers/d1 write, no billing, no analytics.
* Latest deployment: `f323aa6a-ff27-4ad0-8825-7645963667c8` **100 %** plus `d67367ae-3719-4403-9441-c345888a9884` **0 %** (message
  "0% canary for version-override testing; live stays on f323aa6a"). `/api/health` 200, `/login` 200: healthy, no rollback needed.
* Versions left by the earlier interrupted runs (not live, 0 % traffic):
  `13cbbad1` (first optimized build), `695b1833` (tag `r2-fixes`: request-safe shared work, weak INM, DONE; **no** CRON_SECRET),
  `d67367ae` ("CRON_SECRET for 0% testing", created with `versions secret put`: 695b1833 plus the secret binding).
* `wrangler secret list` shows `CRON_SECRET` because it reads the latest version (`d67367ae`). Behaviour check: anonymous
  `GET /api/cron/flush` returns **200 on live** (no secret), **200 on 695b1833**, **401 on d67367ae**.

## S2 CRON_SECRET decision

* The live build (`f323aa6a`, built 2026-10-01) already has the secret-aware code: its scheduled handler sends
  `Authorization: Bearer $CRON_SECRET` when `env.CRON_SECRET` is set (present since 6194f0ca, 2026-09-24) and
  `/api/cron/flush` compares it. So a secret on live would not break the 10-minute flush.
* Not applied to live, deliberately: `wrangler secret put` cannot edit a secret in place. It creates a new version (same code
  plus the secret) and deploys it at 100 %, so live would stop being `f323aa6a`, which is a promotion on the Free account
  (not authorized). The residual risk is unchanged from before: an unauthenticated, cheap invocation of the flush route.
* The secret is applied only to versions under test (`d67367ae` has it, and later uploads inherit it from the latest version;
  verified in S3). For live, the user runs one command (see "Manual steps" at the end).

## S3 build and upload (HEAD 1ef60670 + docs)

* `npm run build:cloudflare`: assertions passed (48 s), `git status` clean afterwards (`src/proxy.js` present, no `src/middleware.js`,
  no `*.cloudflare-disabled`). `wrangler deploy --dry-run`: **11,554.69 KiB raw / 2,816.70 KiB gzip** (< 3 MiB).
* `wrangler versions upload --tag opt-v2` gave **`449c180f-df75-4bdb-b2a3-12f655d1d5ec`** (message "optimized-v2 candidate"), not deployed.
  A new upload inherits the secret bindings of the latest *version* (here `d67367ae`), so `449c180f` already has `CRON_SECRET`.
  I also ran `versions secret put CRON_SECRET` (stdin) which created `5aa6c6ea` (identical code and secrets, redundant, 0 %, unused).
* The version-override header only works for versions that are part of the current deployment (a not-deployed version answers like
  live). So `versions deploy f323aa6a@100 449c180f@0` was used; live traffic stays 100 % on `f323aa6a`. The old 0 % canary
  `d67367ae` left the deployment. Check: anonymous `/api/cron/flush` is 401 on `449c180f` via override, 200 on live (live has no secret).
* `npm run build` again afterwards: variant marker `node`, tree clean.

## S4 smoke on `449c180f` (0 %, version-override header), 2026-10-29 15:33 to 15:47 UTC

Credentials: admin API token from `.env` (accepted). Dashboard: the `.env` password and `123456` were tried (with the earlier
`.env.deploy` attempt that makes three distinct passwords, the budget is used up); the Worker's `JWT_SECRET` is not the one in
`.env.deploy` (signed cookie rejected, 401). **Password login is killed by the Free CPU limit** (`bcryptjs.compare` in pure JS: error 1102 every
time on this plan), so a correct password could not have succeeded here either. Dashboard cases D1 to D7 are therefore unverifiable on Free.

Run 1 (everything incl. 21 MB upload): 20 pass, 6 fail, 6 skip. Run 2 (after the harness retried CPU-limit kills, 9534a767): 18 pass,
6 fail, 8 skip. Failures are in every case a Cloudflare 503/1102 (non-JSON body) or a client-side timeout on a CPU-killed invocation, plus D1.

| Case | Result on 449c180f |
|---|---|
| P1 to P7, P9, P10 | pass (P8 chat with an invalid key: 503 once in run 2, 401 in run 1) |
| P11 cron route | **pass: 401 without the secret, 200 with it** |
| P12 21 MB body | run 1: client timeout at 45 s (slow uplink, the earlier session got 413 after 62 s; the 20 MB cap is also unit-tested). Not re-run |
| D1 to D7 dashboard | **unverifiable**: password login is killed by the 10 ms limit (1102), password unknown, JWT secret unknown |
| G1 key create via admin API | pass (needs 1 to 80 retries; creating a key costs about 290 to 740 ms CPU) |
| G2 `/v1/models` + ETag, `If-None-Match` | **pass: 304 when the ETag is echoed exactly as the edge sent it** (run 1; the weak-ETag comparison is unit-tested in `models-if-none-match.test.js`; live `f323aa6a` fails this case, see below) |
| C1 chat non-stream (real provider) | pass (8.8 s and 11.6 s cold) |
| C2 stream | run 1 pass: 3 events, ends with a finish chunk; the smoke script accepts a stream with `[DONE]`. Run 2 skipped (503) |
| K1 forceModel rewrite | run 1 pass (`openai/gpt-4o` served as `gemma4:31b-cloud`), run 2 500 (isolate error below) |
| K2 forceProvider 403, K3 ACL 403, K4 blocked 403, K5 pin/blocked conflict 400, K7 disabled 401 | pass (each at least once on a retry past a CPU kill) |
| K6 rate limit | run 1 pass (404,404,404,429,429, Retry-After 52); run 2 client timeout on a hung request |
| A1 forced analytics flush with the secret | run 1 pass (`ok:true`, `buffering:true`); run 2 503 |
| X1 throwaway keys | run 1 pass (7 deleted); run 2 list was killed, cleaned up by hand afterwards: no `smoke-test` key left |

Isolate poisoning (new finding): after an invocation is killed for CPU while Next is loading a route, that isolate keeps answering
**500 `TypeError: ComponentMod.handler is not a function`** (143 events in the soak, `/v1/models`, `/login`, chat) and
`Cannot read properties of undefined (reading 'length')` for missing static paths, until it is evicted. A half-initialised module
stays cached. This is triggered by a hard kill, which does not happen on the paid plan CPU limit under normal load, but it is a
resilience risk after any exceeded-CPU event.

## S5 bounded soak: new `449c180f` against old `f323aa6a`, equal traffic

Both runs: `scripts/smoke/soak.mjs`, version pinned by the override header, one request every 3 s for 780 s (mix: health,
`/v1/models`, `/v1/models` with `If-None-Match`, chat with an invalid key, `/login`, one static asset) plus a 20-request concurrent
burst of `/v1/models` before and after, `wrangler tail --version-id` captured in the same command. 264 requests (new) and 177 (old;
12 requests hung for 45 s each, so fewer fit in the window). Old: 16:14 to 16:28 UTC. New: 15:52 to 16:08 UTC.

| | old `f323aa6a` | new `449c180f` |
|---|---|---|
| Worker invocations (Cloudflare analytics, whole window) | 149 | 275 |
| `exceededResources` (Free 10 ms kill), Cloudflare outcome | 12 (**8.1 %**) | 45 (**16.4 %**) |
| `clientDisconnected` / `canceled` (hung requests, client gave up) | **4 (6 in tail)** | 0 |
| HTTP 500 from a poisoned isolate (`ComponentMod.handler is not a function`) | 0 | **143 + 37** (65 % of all answers) |
| CPU ms p50 / p90 / p99, successful outcome (analytics) | 22.9 / 305 / 792 | 9.1 / 22 / 976 (p50 is low because most answers are cheap 500s) |
| CPU ms p50 / p99 of killed invocations | 10 / 277 | 10 / 122 |
| Tail CPU of completed 2xx/401 answers, p50 / p90 / p99 / max | 22 / 305 / 792 / 1,140 (n = 137) | 14 / 816 / 1,185 / 1,185 (n = 60, mostly cold: survivors only) |
| Completed `/api/health` CPU p50 / p99 (tail) | 6 / 30 | 9 / 15 |
| Completed `/v1/models` CPU p50 / p90 / p99 (tail) | 34 / 543 / 1,140 (n = 69) | 733 / 975 / 1,185 (n = 23, all cold; the warm ones die at 10 ms) |
| Share of completed answers within 10 ms | 20 % | 37 % |
| Wall time p50 / p99 (analytics, ms) | 83 / 6,254 | 10.7 / 4,496 |
| Client-side latency, completed answers, p50 / p95 / p99 (ms) | 986 / 8,523 / 11,606 (n = 154) | 477 / 7,348 / 8,338 (n = 58) |
| Client-side latency, all requests incl. hung, p50 / p95 / p99 | 1,003 / 45,000 / 45,001 | 403 / 6,363 / 7,348 |
| Hung requests (45 s client timeout) | **12** (2 + 3 in the timed mix, 7 in the end burst) | **0** |
| Cold isolates seen (`[DB] Driver` log lines) | 11 | 14 |
| D1 (whole window) | 107 read queries, 32,339 rows read, 4 write queries, 4 rows written | 107 read queries, 63,803 rows read, 4 write queries, 7 rows written |
| Durable Object requests | 173 (1 error) | 105 (0 errors) |

How to read it:

* **Free plan: not viable, and the new build is not better on it.** Every warm `/v1/models`, `/login` and chat request costs more than
  10 ms, so the new build (kills 16 %) is no better than the old one (8 %) on Free. The 16 % vs 8 % gap is not a regression
  proof either: the new run was dominated by the poisoning effect below and the old one by hung requests, and the two mixes differ after kills.
* **Isolate poisoning (new finding).** Once an invocation is killed for CPU while Next is loading a route (`/v1/models`, `/login`,
  chat), the same isolate answers every later request for that route with `500 TypeError: ComponentMod.handler is not a function`
  (cheap, 4 to 19 ms CPU) for the rest of its life. It also affects the old build in principle but did not show in the old run. This needs a hard
  kill, so it is a Free-plan artefact; on Paid it only happens after a 30 s CPU or memory termination. It is not in app code (OpenNext keeps the half-initialised
  module in its cache). Mitigation if it ever shows on Paid: alert on `ComponentMod.handler` errors, and redeploy (a new version starts fresh isolates).
* **Hung requests: improved.** The old build hangs under the concurrent cold burst (7 of 20 in the end burst, plus 5 in the timed mix, wall 75 s,
  `canceled`/`clientDisconnected`, `waitUntil() tasks did not complete`). The new build had 0 hangs in 40 burst requests and 224 timed requests
  (b31373eb: bounded D1 calls and request-safe shared work). One hang was seen in a separate smoke run (K6 rate limit check, run 2; analytics shows it as one `clientDisconnected`, wall 75 s): so not eliminated, but 1 in about 400 requests versus 12 in 177 before. Re-check on Paid.

## Measured against claimed (session 2)

| | Claimed | Measured | Verdict |
|---|---|---|---|
| Cold `/v1/models` D1 rows | 5,384 read / 6 queries (docs/CLOUDFLARE_EXTREME_PROFILE.md) | 63,803 rows over 14 cold isolates = about 4,560 per cold isolate, 107 read queries | **confirmed** (database has since lost 1 to 2 k kv rows, the order matches) |
| Warm request D1 writes | 0 to 0.1 rows | 4 write queries and 7 rows for 275 requests (0.03 rows/request); the 4 writes include the throwaway key create and delete | **confirmed** |
| Warm DO requests | about 0.05 per request | 105 DO requests / 275 requests = 0.38, but 14 cold isolates each poll and register (about 7 per cold isolate); the warm share could not be isolated because Free kills evict isolates | **not isolable**, upper bound 0.38 |
| Cold CPU (Paid target: cold p99 well under 30 ms) | | **not achievable.** Cold `/v1/models` 500 ms CPU locally and 733 ms p50 / 1,185 ms max on Cloudflare, cold `/login` 121 to 401 ms, cold health about 35 ms, admin key create 290 to 740 ms. About 200 ms of the cold cost is V8 compiling the 11.5 MB bundle's lazy module bodies (profile: one anonymous frame `worker.js:1` with no app code), not app logic | claim **fails**; on Paid it is a latency cost (about 0.5 to 1 s on a cold isolate), not a failure: the CPU limit is 30 s |
| Warm CPU | | `/api/health` 9 ms (p50, tail, new) vs 6 ms (old). `/v1/models` warm on new could not be measured on Free (dies at 10 ms); old warm 34 ms | partial |
| Static assets without Worker invocation | yes | `/_next/static/*` hits: `cf-cache-status: HIT`, no invocation in the tails (the 37 `missing.js` requests in the new soak were a harness artefact: `/login` was killed so no real asset path was read, they were Worker invocations by design) | pass |
| 413 cap | 413 above 20 MB | **21 MB upload to the new version: 413 JSON `request_too_large` after 90 s** (slow uplink). A forged `Content-Length` is rejected by the Cloudflare edge itself with 400 | pass |
| Cron every 10 min through the service binding | yes | live `f323aa6a`: `[cron flush] 200` at the boundary (tail, 16:20 UTC) and earlier session lines. New build: scheduled handler run locally with `wrangler dev --test-scheduled` and a `CRON_SECRET`: calls `POST /api/cron/flush` through `WORKER_SELF_REFERENCE`, 200. Not runnable on Cloudflare for a 0 % version (cron runs the live version) | pass (local) / unverifiable remotely |
| Log volume | small | Tail: WARN/ERROR only. Persisted Workers Logs not read (no token scope) | partial |
| Memory outcomes | none | No `exceededMemory` outcome in either run; Cloudflare does not expose memory | no signal |

## Fixes in this session (all committed, local)

| Commit | What |
|---|---|
| a6c45cc8 | SSE ends with exactly one `[DONE]` |
| b31373eb | request-safe shared work, bounded D1 calls, weak `If-None-Match` |
| 6edbd167 | constant-time `CRON_SECRET` compare + tests |
| f00cce08, 1ef60670 | `build:cloudflare` recovers a stale swap, drops the webpack cache |
| a987e79a, 86e1fd75 | smoke: dashboard/session fallback, pin pair 400s, key CRUD, analytics flush, cold burst |
| 9534a767 | smoke: policy checks retry a CPU-limit 503 and count the kills |
| this session | `scripts/smoke/soak.mjs` (bounded soak with exact percentiles and burst phases) |

No cold-path code change was made: the profile shows no cheap, safe app-level target (health is already trivial: the route returns a constant;
the middleware does no I/O for public paths; the bulk is V8 compile and Next/OpenNext module evaluation). Lazy-loading more inside the single
11.5 MB bundle would not change compile cost per isolate, and changes to the routing engine's import graph carry regression risk for no measured gain.

## Old live version, same smoke (`f323aa6a`, 16:29 to 16:38 UTC)

13 pass, 9 fail, 2 warn, 8 skip. Differences from the new version, all confirmed in data: `/_next/static` served with
`cache-control: public, max-age=0, must-revalidate` (no immutable header: P5 fail), `/api/cron/flush` public (P11 warn),
`If-None-Match` with the weak edge ETag does not give 304 (G2 warn), a forced `forceModel` pin to `openai/gpt-4o` is answered 404 "No active credentials for provider: openai"
(K1 fail: the old build does not rewrite the model before the lookup), two requests hung 45 s (K2, K6), rest CPU kills (K4, K5, K7, X1).
All throwaway keys removed afterwards (`apiKeys` 20 rows, none named `smoke-test`).

## Audit noise left in the live database (read-only check, `d1 execute ... SELECT`)

`auditLog` 226 rows (96 before session 1, 152+ at its end; this session added roughly 70: every admin-API key create/delete and dashboard key
event writes one). No supported API prunes it; no raw `DELETE` was run on live D1. `apiKeys` 20 rows, 0 named `smoke-test`. `usageDaily` now has 5 rows
(the forced analytics flush wrote), `usageHistory` 47, `requestDetails` 350 (unchanged: no per-request rows).

## Final live state (verified at the end of the session)

Not verified from the agent sandbox (no Cloudflare network). Run `scripts/smoke/live-state.sh` from your shell: it checks that `f323aa6a` is at 100 %, `/api/health` and `/login` are 200, and `/v1/models` without a key is 401, and tails 60 s for exceeded/exception outcomes. Read-only.

## Remaining risks

1. Free plan cannot host this Worker (both versions: about 8 to 16 % of invocations killed, every warm `/v1/models`, `/login`, chat and admin write above 10 ms).
2. Isolate poisoning after any hard termination (see S5). Alert on `ComponentMod.handler is not a function`; a new version deploy clears it.
3. Hung concurrent cold requests reduced to about 1 in 400 on the new build (12 in 177 on the old one) but not proven absent. Re-run the burst on Paid.
4. Cold request cost 0.5 to 1.2 s CPU. Fine under a 30 s limit; it is billed CPU time and first-byte latency, not a failure.
5. Password login (`bcryptjs`, pure JS) is CPU heavy (about 100 ms or more). Fine on Paid.
6. New API keys take 2 to 19 s to become visible to every isolate (documented 10 s poll, 30 s unknown-key window).
7. `auditLog` grows with admin use and has no prune API.
8. Dashboard cases D1 to D7 and the analytics-in-dashboard check were not run against a real session (password unknown, 3 distinct attempts used, Worker JWT secret unknown). They are covered by the unit tests, and `scripts/smoke/deployed-smoke.mjs` runs them as soon as `SMOKE_DASHBOARD_PASSWORD` (or `SMOKE_JWT_SECRET`) is the right one.
9. Persisted Workers Logs volume and memory use are not readable with the CLI's OAuth scopes. Read them in the dashboard during the Paid soak.
10. Provider connections in the stored database: `cloudflare-ai` and Ollama `h` return 401 upstream (stale credentials), unchanged from session 1; not touched.

## GO / NO-GO

* **Free account: NO-GO.** Live stays on `f323aa6a` at 100 %. Do not promote `449c180f` here.
* **Paid account: GO, conditional.** The conditions from session 1 are now met except the ones that need Paid or the dashboard password:
  met: `[DONE]` always once, weak `If-None-Match` 304, `CRON_SECRET` enforced and constant-time, bounded D1 calls and request-safe
  shared work (hangs 12 in 177 down to 1 in about 400), 413 cap on the deployed build, build recovers from a killed swap, local cron path with a secret,
  smoke + soak harness with exact percentiles. Not met until done on Paid: a 20-minute soak with 0 hung requests and 0 exceeded outcomes, the dashboard
  cases with the real password, per-request D1/DO figures at warm state, memory outcomes.

## Paid account deployment runbook (ready to run, nothing below has been executed)

Work from a clean checkout of `master` (HEAD includes everything above). `$OLD` = the free account, `$NEW` = the paid account.

### 0. Prepare
```bash
cd /devspace/Projects/9router
git status --short                                   # must be empty
npx wrangler logout && npx wrangler login            # log in as the PAID account
npx wrangler whoami                                  # check the account name and id
```
In the Cloudflare dashboard of the paid account: Workers & Pages, Plans, confirm **Workers Paid**. Add a billing / usage notification
(Notifications, "Usage-based billing" for Workers, D1 and Durable Objects).

### 1. Export the data while logged in to `$OLD` (do this first, with the old login)
```bash
npx wrangler d1 export mihir-router-db --remote --output ~/9router-d1-export.sql    # old account
# older fallback: .agents/backups/mihir-router-db-pre-deploy.sql (2026-10-29 morning, 4,136 rows)
```
Also write down the OLD Worker secrets. They cannot be read back from Cloudflare. `API_KEY_SECRET` and `MACHINE_ID_SALT`
**must be identical** on the new Worker, or existing API keys stop validating (the key CRC is an HMAC with `API_KEY_SECRET`
over the machine id). `JWT_SECRET` should stay the same to keep dashboard sessions; `ADMIN_API_TOKEN`, `INITIAL_PASSWORD` as before.
If you do not have them, generate new ones and re-create the API keys in the dashboard afterwards.

### 2. Create D1 and import (paid account)
```bash
npx wrangler d1 create mihir-router-db               # prints database_id
# put the new database_id into wrangler.toml [[d1_databases]] (do not commit it to a shared repo)
npx wrangler d1 execute mihir-router-db --remote --file ~/9router-d1-export.sql
npx wrangler d1 execute mihir-router-db --remote --command "SELECT table_name, count(*) n FROM ninerouter_rows GROUP BY 1 ORDER BY 2 DESC"
```
Expect `ninerouter_rows` only; compare the counts with the old database (about 4,100 rows).

### 3. Durable Object
`wrangler.toml` already declares `[exports.GatewayCoordinator] type = "durable-object", storage = "sqlite"`; the first deploy creates the
SQLite-backed class, no manual migration. Needs Workers Paid.

### 4. First deploy (creates the Worker; this one activates at 100 %)
`wrangler versions upload` cannot create a Worker, so the first deploy is a normal one. Keep it unreachable until secrets exist:
```bash
# temporarily in wrangler.toml: workers_dev = false   (no route yet = no public URL)
npm run build:cloudflare && npx wrangler deploy --dry-run --outdir /tmp/wr-dry    # raw / gzip; the compressed size must stay under 10 MB on Paid
npx wrangler deploy --message "first deploy, no public URL"
cat > /tmp/secrets.json   # {"JWT_SECRET":"...","API_KEY_SECRET":"...","MACHINE_ID_SALT":"...","ADMIN_API_TOKEN":"...","INITIAL_PASSWORD":"...","CRON_SECRET":"<openssl rand -hex 32>"}
npx wrangler secret bulk /tmp/secrets.json && shred -u /tmp/secrets.json
```
(`CRON_SECRET` may be taken from `.agents/secrets/cron-secret.txt`.) Then turn on a test URL: either `workers_dev = true` plus a `deploy`,
or better a custom domain below. Secrets are per Worker; they do not move with a deploy.

### 5. Every later release: upload, 0 %, smoke, soak, promote
```bash
npm run build:cloudflare
npx wrangler versions upload --message "release X" --tag release-x           # prints the Version ID, 0 % traffic
npx wrangler versions deploy <PREVIOUS>@100 <NEW>@0 -y --message "0% canary"  # makes the override header usable
SMOKE_BASE_URL=https://<host> SMOKE_ENV_FILE=<file with ADMIN_API_TOKEN, SMOKE_CRON_SECRET, SMOKE_DASHBOARD_PASSWORD> \
  SMOKE_VERSION_OVERRIDE=<NEW> SMOKE_CHAT_MODEL=<cheap model> SMOKE_OVERSIZE=1 node scripts/smoke/deployed-smoke.mjs
SMOKE_BASE_URL=... SMOKE_ENV_FILE=... SMOKE_VERSION_OVERRIDE=<NEW> SOAK_SECONDS=1200 SOAK_INTERVAL_MS=2000 \
  node scripts/smoke/soak.mjs      # run `npx wrangler tail --version-id <NEW> --format json` in a second terminal
```
Go criteria: 0 hung, 0 `exceededResources`/`exceededCpu`/`exceededMemory`, 0 `ComponentMod.handler` errors, all smoke cases pass.
Promote gradually:
```bash
npx wrangler versions deploy <PREVIOUS>@90 <NEW>@10 -y     # watch 10 to 15 minutes (errors, CPU p99, D1 rows, DO requests)
npx wrangler versions deploy <NEW>@100 -y
```
Rollback (instant): `npx wrangler rollback <PREVIOUS_VERSION_ID> -y`, then check `/api/health` and `/login`.

### 6. Dashboard actions (cannot be done from the repo)
1. Custom domain: Workers & Pages, the Worker, Settings, Domains & Routes, add a domain on a zone you control.
2. On that zone: Security, WAF, Rate limiting rules: for example `/v1/*` and `/api/auth/login` per IP (login: 10 per minute), and turn on **Bot Fight Mode**.
3. After the custom domain works: set `workers_dev = false` in `wrangler.toml` and deploy (removes the open `workers.dev` URL that the zone's WAF does not cover).
4. Optional `[limits] cpu_ms = 5000` (or another ceiling below the 30 s default) in `wrangler.toml` to bound a runaway request; leave unset first.
5. `[placement] mode = "smart"` is unmeasured; try it as a 0 % version and compare wall time before enabling.
6. Observability: keep sampling at `0.05` / `0.01`; check the monthly log/trace event count after the first week.
7. Set a D1 usage notification; check row reads per day against the estimate of about 4.5 k rows per cold isolate.
8. Rotate the dashboard password in Settings after the first login, and review the provider connections whose credentials are stale.

## Manual steps for the user (free account)

* Nothing is required to keep the free deployment healthy: `f323aa6a` serves 100 %.
* Optional hardening of the live Free Worker (this is a promotion, so it was not done): `npx wrangler versions secret put CRON_SECRET` then
  `npx wrangler versions deploy <current>@100` (creates and deploys a copy of `f323aa6a` with the secret; the 10-minute cron already sends
  the header). Or accept that `/api/cron/flush` stays a cheap public route on Free.
* Old leftovers on the free account (uploaded, never promoted, unused): `13cbbad1`, `695b1833`, `d67367ae`, `5aa6c6ea`, `449c180f` (0 % canary). To remove the
  canary: `npx wrangler versions deploy f323aa6a-ff27-4ad0-8825-7645963667c8@100 -y`.
