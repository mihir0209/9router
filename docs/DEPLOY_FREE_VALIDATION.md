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
