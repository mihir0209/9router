# Paid account deployment: Worker `router` (2026-10-29)

Progress log, written stage by stage. No secret values appear in this file. Nothing is pushed.

## P0 preflight

* `npx wrangler whoami`: account **R8devsin@gmail.com's Account**, id `1da22c19548115accf1b98ab7c562b2c` (the only account on the login).
  The personal free account (`60bf355a...`, 'Mihir@cognixai.in's Account') is a different account, so deployment may proceed.
  `CLOUDFLARE_ACCOUNT_ID` is exported for every wrangler command to rule out a wrong-account call.
* `workers.dev` subdomain of this account: `genzzz`, so the Worker URL will be `https://router.genzzz.workers.dev`.
* Worker `router` does not exist on this account (`deployments list` / `versions list`: error 10007 "Worker does not exist"). `wrangler d1 list`: no databases. Nothing to overwrite.
* Plan: **unverified**. The OAuth token has no billing scope (`/subscriptions`: 10000 Authentication error). `workers/account-settings` shows `default_usage_model: standard`, which is consistent with Paid but not proof. Confirm in the dashboard (Workers & Pages, Plans).
* Env mapping (from key names only). `.env.deploy` is the Antideploy file. Keys: NODE_ENV, NODE_OPTIONS, JWT_SECRET, API_KEY_SECRET, MACHINE_ID_SALT, ADMIN_API_TOKEN, INITIAL_PASSWORD, NEXT_PUBLIC_BASE_URL, MODELS_LIST_SOURCE, DATA_DIR. `.env` has INITIAL_PASSWORD, ADMIN_API_TOKEN.

| Source key | Target | Why |
|---|---|---|
| JWT_SECRET, API_KEY_SECRET, MACHINE_ID_SALT, ADMIN_API_TOKEN, INITIAL_PASSWORD | Worker secrets | read by the guard and the app (`process.env` is filled from the Worker env once per isolate) |
| CRON_SECRET | Worker secret, **newly generated** (not in `.env.deploy`), stored in git-ignored `.agents/secrets/router-cron-secret.txt` (chmod 600); not the personal Worker's secret | without it `/api/cron/flush` is public |
| MODELS_LIST_SOURCE=cached | `[vars]` in `wrangler.router.toml` | not secret; avoids a provider fan-out on `/v1/models` |
| NODE_ENV | `[vars]` (already) | |
| NEXT_PUBLIC_BASE_URL | **not used**: its value is the personal `mihir-router.antideploy.app` host and it is a build-time value | no personal host may reach the paid build |
| NODE_OPTIONS, DATA_DIR | not used | Node-only |

## P1 config

* New `wrangler.router.toml` (copy of `wrangler.toml`; the personal `wrangler.toml` is untouched): `name = "router"`, service binding `WORKER_SELF_REFERENCE` -> `router`,
  D1 `router-db` (binding `DB`), `GatewayCoordinator` Durable Object with `[exports.GatewayCoordinator] type = "durable-object", storage = "sqlite"` (same style as `wrangler.toml`),
  cron `*/10 * * * *`, observability 0.05 / 0.01, `workers_dev = true`, vars NODE_ENV / NEXTJS_ENV / NINEROUTER_RUNTIME and `MODELS_LIST_SOURCE = "cached"` (from `.env.deploy`). No `ORIGIN`.
* Personal-name audit: `mihir` occurs only in `wrangler.toml` (free config), `scripts/smoke/live-state.sh` (default free URL, now overridable) and a usage example in `deployed-smoke.mjs` (made generic).
  `scripts/build-cloudflare.mjs`, `src/`, `open-sse/`, `open-next.config.ts`, `next.config.mjs` contain no worker name or workers.dev host; the cron handler uses the service binding and falls back to `env.ORIGIN` only. The built worker is grepped again after the build (P3).
* `scripts/build-cloudflare.mjs` accepts `--config <file>` (or `NINEROUTER_WRANGLER_CONFIG`) and passes it to `opennextjs-cloudflare build`, which reads the Wrangler config. Without it nothing changes.
* New npm scripts: `build:router` (build only) and `deploy:router` (`build-cloudflare.mjs --config wrangler.router.toml && wrangler deploy --config wrangler.router.toml`; the build runs once).
## P2 D1: BLOCKED (permissions), stopped before any write

* `npx wrangler d1 create router-db` on account `1da22c19...` failed: **`Authentication error [code: 10000]`** on `POST /accounts/1da22c19.../d1/database`.
  Nothing was created: `d1 list` is empty, Worker `router` does not exist, no secret was set, no deploy was attempted.
* Cause (read-only checks with the wrangler login): the login is `mihir@cognixai.in`, which is only a **member** of `R8devsin@gmail.com's Account`
  with roles Zone Settings Read/Admin, AI Gateway Admin, OAuth App Registrations R/W, IP Addressing R/W, Page Rules R/Admin, Resource Library Admin, Zone Versioning, Minimal Account Access,
  HTTP Applications, Trust and Safety, DNS and **Administrator Read Only**. The membership permission map shows `worker: edit=false` and has no `d1` entry
  (the OAuth scopes themselves include `workers (write)` and `d1 (write)`, so the account role is what blocks the write). The same login is Super Administrator on the personal account.
  Wrangler printed "Contact account super admin to change your permissions".
* Existing Workers on the paid account (not touched, not mine): `lordr`, `my-portfolio-on-cf`.
* To continue, one of: (a) `npx wrangler logout && npx wrangler login` as `R8devsin@gmail.com` itself; (b) the account owner gives `mihir@cognixai.in` the roles
  *Workers Platform Admin* and *D1 Admin* (or Administrator) in Manage Account, Members; (c) an API token of that account with Workers Scripts Edit, D1 Edit, Account Settings Read
  exported as `CLOUDFLARE_API_TOKEN` (and `CLOUDFLARE_ACCOUNT_ID=1da22c19548115accf1b98ab7c562b2c`).

## P3 offline pre-validation (no Cloudflare write)

Because the deploy is blocked, the parts that need no account access were run so the next attempt starts from a verified state:

* `npm run build:router` (`build-cloudflare.mjs --config wrangler.router.toml`): all build assertions passed (47 s); afterwards `git status` clean, `src/proxy.js` present, no `src/middleware.js`, no `*.cloudflare-disabled`.
* `wrangler deploy --config wrangler.router.toml --dry-run --outdir /tmp/wr-dry`: **11,557.80 KiB raw / 2,818.88 KiB gzip** (Paid limit 10 MiB compressed, so ample). Bindings resolved: `GATEWAY_COORDINATOR` (DO), `DB` (`router-db`, placeholder id until D1 exists), `WORKER_SELF_REFERENCE` -> `router`, `ASSETS`, vars NODE_ENV / NEXTJS_ENV / NINEROUTER_RUNTIME / MODELS_LIST_SOURCE.
* The generated `.open-next/worker.js` and the wrangler bundle contain **no** `mihir`, `themihir` or `antideploy` string.
* `npm run build` afterwards: `.next/standalone/.9router-build-variant` = `node`, tree clean.
* The `database_id` in `wrangler.router.toml` is still the all-zero placeholder and MUST be replaced after `wrangler d1 create router-db`.

---
## P2 D1 (resumed after the user granted the roles)

* `whoami`: still `mihir@cognixai.in`, account **R8devsin@gmail.com's Account** (`1da22c19...`) only; `d1 list` worked (empty) and `d1 create router-db` succeeded (region APAC).
* D1 `router-db`, id `3bc7cca5-54f8-454f-bcdf-87413ff396c0`, written to `wrangler.router.toml`.
* Import of `.agents/backups/mihir-router-db-pre-deploy.sql` (export of the user's own personal DB from 2026-10-29 12:32, unfiltered, one `d1 execute --file`): 4137 changes, 8275 rows written.
  `SELECT table_name, COUNT(*) ... GROUP BY 1` equals the backup's per-table counts: _meta 1, apiKeys 20, auditLog 96, combos 10, kv 3502, providerConnections 62, providerNodes 17, proxyPools 25, requestDetails 350, settings 1, usageDaily 5, usageHistory 47 (total 4136).
* Note: later changes made on the personal DB after that export are not included in `router-db`. The data contains provider credentials; contents were never printed.

## P3 secrets, build, deploy

* Secrets were set **before** the first code deploy with `wrangler secret bulk` (JSON on stdin, never argv): JWT_SECRET, API_KEY_SECRET, MACHINE_ID_SALT, ADMIN_API_TOKEN, INITIAL_PASSWORD (from `.env.deploy`) and CRON_SECRET (generated). Wrangler created the Worker `router` as a secrets-only placeholder (version e87e5cc3). So the real code never served without secrets. NEXT_PUBLIC_BASE_URL was not copied.
* `npm run build:router`: all assertions passed, tree clean afterwards (src/proxy.js present, no src/middleware.js, no *.cloudflare-disabled). Dry run: 11,557.78 KiB / 2,818.38 KiB gzip, Worker startup 14 ms. Built worker and wrangler bundle: no `mihir`, `themihir`, `antideploy`.
* Deploy 1: version `0477bfab-7820-4147-92bd-df1827b5a2b3`. Wrangler warned that Preview URLs default to enabled; added `preview_urls = false` to `wrangler.router.toml` and redeployed the same build.
* **Current version: `830cb6ff-92b5-4bf8-85f8-19e316796dc2`** (100%) at https://router.genzzz.workers.dev, cron `*/10 * * * *`, DO `GatewayCoordinator` created, bindings DB / GATEWAY_COORDINATOR / WORKER_SELF_REFERENCE -> router / ASSETS.
  Previous versions: 0477bfab (same code, preview URLs on), e87e5cc3 (secrets only placeholder).

## P4 functional smoke (version 830cb6ff, https://router.genzzz.workers.dev)

* `live-state.sh` (LIVE_STATE_CONFIG=wrangler.router.toml): health 200, login 200, `/v1/models` without key 401, cron 401, six secret names present. Its "100%" grep failed because wrangler now prints `(100%) <id>`; fixed in its own commit (`fix(smoke): live-state accepts both wrangler deployment line orders`).
* `deployed-smoke.mjs` run 1 (with the 21 MB oversize upload): 29 pass, 2 fail. Both failures were harness assumptions, not the app: D6 used the alias prefix `cf` as forceProvider (the app correctly requires the provider id `cloudflare-ai`),
  D5's "bare pin" used a leaf containing a slash (`@cf/...`), which is not a bare pin. Fixed in the script (`SMOKE_CHAT_PROVIDER`, fixed bare pin `gpt-4o` vs blocked `openai/gpt-4o`), commit `test(smoke): pin cases work for aliased chat models`.
* Run 2 (after the fix): 30 pass, 1 skip (oversize, passed in run 1: 413 in 35.9 s including the 21 MB upload).
* Dashboard password: the .env.deploy value and the .env value were both rejected (HTTP 401; the imported settings row holds its own password hash). 2 attempts, no more. Login/logout, D1-D7 pages and key CRUD were exercised with a locally signed session cookie using the Worker's JWT secret (own deployment, in memory only). Password login on the Paid plan (bcrypt) is therefore **not** verified; the user must log in with the password of the old (imported) settings.
* Chat via `cf/@cf/meta/llama-3.2-1b-instruct` (connection `cloudflare-ai`): non-stream 200 (818-1338 ms), stream 200 (ttfb about 512 ms). `sn/` returned an upstream 404 from the user's BYOK node (their provider, not this deployment).

## P4/P5 final results (2026-10-30)

**Account** R8devsin@gmail.com's Account (`1da22c19...`). **Worker** `router`, https://router.genzzz.workers.dev, cron `*/10 * * * *`, `workers_dev = true`, `preview_urls = false`.
**Live version: `4c0b49ff-63ae-4d59-984f-2d22d1d23e54`** (100%), built from the committed source. Earlier versions of this Worker: 830cb6ff (same code, used for the first smoke and the 17-minute soak), 6713873c (a rolled-back experiment, see below), 0477bfab, e87e5cc3 and 77618db2 (secrets-only placeholders).
**D1** `router-db`, id `3bc7cca5-54f8-454f-bcdf-87413ff396c0` (APAC). **Secrets set (names):** JWT_SECRET, API_KEY_SECRET, MACHINE_ID_SALT, ADMIN_API_TOKEN, INITIAL_PASSWORD, CRON_SECRET.

### Test matrix (final version 4c0b49ff: smoke run 3; 830cb6ff: runs 1 and 2, same code)

| Area | Result |
|---|---|
| P1-P11 public surface (health, redirects, login page, immutable `/_next/static`, auth status, 401s, admin API token check, cron 401 without / 200 with secret) | pass |
| P12 413 above the 20 MB cap (JSON error) | pass (run 1, 35.9 s incl. the upload; skipped in run 3) |
| D1-D6 login/logout, pages endpoint/providers/usage/profile, dashboard APIs, key CRUD, POST/PUT mismatched forceProvider/forceModel -> 400 | pass, using a locally signed session cookie. **Password login not verified**: the .env.deploy and .env passwords were both rejected (2 attempts); the imported settings row holds its own hash |
| G1-G2 `/v1/models` with a key, ETag, `If-None-Match` 304 (strong; the edge sent no weak ETag in these runs) | pass |
| C1 chat non-stream, C2 chat stream (4 SSE events, ends in `[DONE]`), via `cf/@cf/meta/llama-3.2-1b-instruct` | pass (3 real calls per run, max_tokens <= 16) |
| K1-K7 forceModel rewrite, forceProvider 403, allowedModels 403, blockedModels 403, pin vs blocked 400, rate limit 429 + Retry-After, disabled key 401 | pass |
| A1 forced analytics flush + usage endpoints | pass (endpoint answers; see risk 2 for what it can and cannot show) |
| X1 throwaway keys deleted (also the ones from my probes; `apiKeys` has 0 rows named smoke-test) | pass |
| Cold burst, 20 concurrent `/v1/models`, twice per soak, 3 soak/burst runs | 0 hung, 0 non-200 |
| Soak 1 (830cb6ff): 17 min, 479 requests, 1 per 2 s, `wrangler tail` captured | 0 hung, 0 CPU kills, 0 app 5xx; 509 tail events all outcome `ok`, 0 exceptions, 0 `ComponentMod.handler is not a function`; cron fired at 05:50 and 06:00 and logged `[cron flush] 200` through the service binding |
| Soak 2 (4c0b49ff): 4 min, 215 requests, 1 per s, tail | 235 events all `ok`, 0 exceptions, 0 poisoning, 0 hung |
| Full vitest (`cd tests && npx vitest run`) | 501 files passed, 24 skipped; only the two known load failures (`unit/db-benchmark.test.js`, `unit/embeddings.cloud.test.js`) |

### Client-side latency (ms, nearest rank, measured from this machine, Worker colo not controlled)

| Sample (soak 1, n = 479) | p50 | p95 | p99 | max |
|---|---|---|---|---|
| all requests | 326.8 | 2024.8 | 3353.8 | 3510 |
| `/api/health` (n 74) | 283.4 | 453.5 | 470.4 | 470.4 |
| `/v1/models` (n 73) | 355.2 | 588.3 | 748.7 | 748.7 |
| `/v1/models` + If-None-Match (n 73) | 285.7 | 438.8 | 757.5 | 757.5 |
| chat, invalid key (n 73) | 349.9 | 487.3 | 573.7 | 573.7 |
| `/login` (n 73) | 275.6 | 438.9 | 494.2 | 494.2 |
| static asset (n 73) | 274.8 | 454.6 | 710.3 | 710.3 |
| cold burst of 20 (start / end) | 2019 / 3334 | 3318 / 3413 | 3506 / 3510 | 3506 / 3510 |

Soak 2 (n = 215): p50 351.7, p95 3138, p99 3901.7, max 4364 (the two cold bursts account for the tail). Real chat: non-stream 818-1338 ms, stream first byte 512-514 ms. Admin key create about 710 ms, delete about 790 ms.

### Cloudflare numbers (GraphQL analytics, account `1da22c19...`, script `router`)

| Window | Invocations | Errors | Outcomes | CPU p50 / p99 / p99.9 | Wall p50 / p99 | Subrequests | D1 rows read / written (queries) | DO requests |
|---|---|---|---|---|---|---|---|---|
| soak 1 (05:47-06:06) | 416 | 0 | all `success` | 9.5 ms / 574 ms / 803 ms | 13.2 ms / 2.12 s | 101 | 38,208 / 7 (112 r, 4 w) | 99 |
| soak 2 (06:28-06:34) | 216 | 0 | all `success` | 13.7 ms / 942 ms / 1.12 s | 19.9 ms / 2.64 s | 69 | 57,407 / 10 (118 r, 6 w) | 68 |
| whole session (05:30-06:35, includes the D1 import) | 1,013 | 0 | all `success`, no exceededCpu / exceededMemory / exception | 11.6 ms / 694 ms / 1.22 s | 16.8 ms / 2.12 s | 635 | 145,514 / 8,511 (400 r, 140 w) | 592 |

`wrangler d1 info` (24 h): 360 read queries, 126,368 rows read, 8,508 rows written, 3.39 MB. The 8,508 writes are the import (8,275, which counts index maintenance) plus about 230 from key create/delete audit rows, secret-less admin writes and smoke runs.
Counts are from Cloudflare's sampled-free adaptive tables; windows overlap with my probes, so treat them as order of magnitude. Workers Logs volume and memory use are not readable with the CLI's OAuth scopes (read them in the dashboard).

### Measured versus claimed

| Claim (docs/CLOUDFLARE_EXTREME_PROFILE.md, DEPLOY_FREE_VALIDATION.md) | Measured on Paid | Verdict |
|---|---|---|
| Cold `/v1/models` reads about the whole table (5,384 rows in the doc's 5.4 k dataset) | about 4,100 rows per cold isolate (57,407 rows over 14 cold isolates, dataset 4,136 rows) | consistent |
| Warm requests read about 0 D1 rows | soak-1 reads came only from cold isolates; D1 read queries 112 for 416 invocations | consistent |
| Analytics add no per-request D1 writes (2 rows per flush) | 7 and 10 rows written in two soaks including key create/delete audit rows | consistent |
| Cold request 0.5 to 1.2 s CPU, fine under 30 s | CPU p99 0.57 to 0.94 s, max p99.9 1.2 s | consistent |
| Free-plan CPU kills (8 to 16 % of invocations) do not occur on Paid | 0 kills, 0 exceeded outcomes in 1,013 invocations | confirmed |
| Hung concurrent cold requests about 1 in 400 on the new build, to re-run on Paid | 0 of 80 burst requests and 0 of 694 soak requests hung | no hang seen (not proof of absence) |
| No isolate poisoning (`ComponentMod.handler is not a function`) | 0 in 744 tail events | not observed |
| Cron flush works through the service binding | `[cron flush] 200` at 05:50 and 06:00 | confirmed |

### Findings

1. **Fixed (tooling only):** `live-state.sh` could not recognise the current wrangler `(100%) <id>` line; `deployed-smoke.mjs` pin cases assumed provider prefix == provider id. No application bug was found.
2. **Analytics visibility (design limit, documented, not fixed):** the usage buffer is per isolate. The cron tick and a dashboard read land on an arbitrary isolate, so the dashboard `usage` pages showed 0 requests for today although chats had run, and `usageDaily` has no new row. I reproduced this locally with `wrangler dev` (one isolate): streaming chats were buffered (`lifetimeRequests: 2`) and written by the flush. Two further observations:
   * The non-stream `cloudflare-ai` (`cf/`) response contains no usage object, so `extractUsageFromResponse` returns null and nothing is counted for that call shape (the 2,018 prompt tokens the client sees are the estimate plus the 2,000-token margin). Other providers return usage.
   * I tried registering `saveRequestUsage` with `ctx.waitUntil` (version 6713873c); it changed nothing, so I reverted it. Live runs the committed code only.
   If the dashboard must show complete usage, consider flushing from every isolate (for example flush on a short timer inside a request via waitUntil) or aggregating in the Durable Object. This is a design change, so I did not make it.
3. **Imported data is a snapshot from 2026-10-29 12:32.** Changes on the personal DB after that are not in `router-db`. The stored dashboard password differs from the .env.deploy value.
4. Stale provider connections `cloudflare-ai` (its non-stream response still works for chat) and Ollama `h` are unchanged in the imported data. A `sn/` model returned an upstream 404 from the user's own BYOK node.

### Rollback

First deploy had no previous version; now `npx wrangler deployments list --config wrangler.router.toml`, then
`npx wrangler rollback 830cb6ff-92b5-4bf8-85f8-19e316796dc2 --config wrangler.router.toml` (same code as live) restores the earlier version. Do not roll back below 0477bfab (placeholder versions have no code).
Deleting the Worker or `router-db` is not part of any rollback and was not done.

### Dashboard actions the user must still do

1. Workers & Pages, Plans: confirm **Workers Paid**; add a usage / billing notification for Workers, D1 and Durable Objects.
2. Attach a custom domain, add WAF rate-limiting rules and Bot Fight Mode, then set `workers_dev = false` in `wrangler.router.toml` and redeploy (zone rules do not cover workers.dev).
3. Optionally set `[limits] cpu_ms` and try Smart Placement (D1 is in APAC; the measured latency here was from a distant client).
4. Log in with the password stored in the imported settings; rotate the password and, if desired, the ADMIN_API_TOKEN. All secrets were taken from .env.deploy and passed via stdin only.
5. Re-test or remove the stale provider connections `cloudflare-ai` and Ollama `h`.
6. Decide about the per-isolate analytics limit (finding 2).

### Local state

`npm run build` node variant (`.next/standalone/.9router-build-variant` = `node`), `src/proxy.js` present, no `src/middleware.js`, no `*.cloudflare-disabled`, tree clean, nothing pushed, no secrets committed. `~/.9router/db/data.sqlite` untouched; the personal account and `mihir-router` were not touched.
