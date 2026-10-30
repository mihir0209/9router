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


---
# Providers wipe, D1 reload (2026-10-30)

Staged log. No secret value appears here; backup contents were never printed.

## B0 inspection (read-only)

* `wrangler whoami`: only `R8devsin@gmail.com's Account` (`1da22c19...`), so the paid account. `CLOUDFLARE_ACCOUNT_ID` is exported for every command.
* New backup `/home/ghostrider/Downloads/9router-cognixai-cloud.json` (868,065 bytes, file mtime 13:31 IST). Top-level keys: settings, providerConnections, providerNodes, proxyPools, apiKeys, combos, modelAliases, customModels, mitmAlias, pricing (no version/format field, the app's export format).
  Counts: settings 1 (42 keys, **no `password`**, no `oidcClientSecret`), providerConnections **62** (all ids distinct), providerNodes 18, proxyPools 0, apiKeys 7, combos 11, modelAliases 1084, customModels 1888 (distinct composite keys 1888), mitmAlias 0, pricing 0.
  `validateBackup()` (src/lib/db/mergeBackups.js): valid. The backup is **not** empty and uses the standard field names, so a missing/renamed `providerConnections` does not explain the wipe.
* Live `router-db` at the start of this stage (before any change by me), per table: _meta 1, auditLog 158, kv 3, requestDetails 350, usageDaily 6, usageHistory 47 (565 rows). **settings, providerConnections, providerNodes, proxyPools, apiKeys, combos: 0 rows; kv lost all modelAliases / customModels rows.**
  Before the user's import it held the old snapshot (kv 3502 = customModels 2414 + modelAliases 1085 + disabledModels 1 + gemini_thought_signatures 2, providerConnections 62, providerNodes 17, proxyPools 25, apiKeys 20, combos 10, settings 1, ...). The import wiped exactly the tables `importDb` wipes (3,634 rows) and kept the other tables and the 3 kv rows of scopes it does not touch.
* Evidence from Cloudflare D1 analytics (account GraphQL, no payloads): between 06:35 and 08:15 UTC the database saw about 3,129 single-row `DELETE ... WHERE table_name = ? AND row_key = ?` statements and **no `INSERT` at all** (the analytics under-report the 3,634 by about 500; the final table state is complete). Worker invocations in that window: all `success`, 0 errors.

### Root cause (as far as it can be shown)

`importDb` replaces the database: it deletes every row of settings, providerConnections, providerNodes, proxyPools, apiKeys, combos and four kv scopes, then inserts the payload. It **accepts any object**. The route also accepts `{ password }` alone.
I reproduced the live end state exactly with the real `importDb` + `cloudflareMemoryAdapter` over a D1 stand-in loaded with the old snapshot: an import whose payload carries none of the tables deletes 3,634 rows, inserts nothing and leaves `_meta/auditLog/kv(3)/requestDetails/usageDaily/usageHistory` untouched, which matches the live D1 table by table.
The dashboard can produce such a payload: in **merge** mode (the default) the page posts `{ ...merged, password }`, and `mergeBackups()` returns `merged: null` whenever `validateBackup` finds a `null` table in the base or the incoming file, so the page posts only `{ password }`. The server answered 200 and the dashboard reported success. Which exact file/click produced this on 2026-10-30 cannot be recovered (the request body is not logged and the file that was used is not known to me; the file now in Downloads validates fine).

Ruled out by replay (same snapshot, same new backup, all persist tasks allowed to finish): a full **replace** import and a **merge** import both end with the backup's rows (`providerConnections 62`, `kv` complete). So the persistence layer works when it runs to completion.

Real weaknesses in the Cloudflare path that can also lose data, independent of the trigger above:

1. **No import validation**: an empty or table-less payload is a silent "delete everything" (above).
2. **Delete before insert**: `persistDirtyRows` sent the row deletes first and the upserts after, so any failure or cancellation between the two leaves a database with the old rows gone and the new ones missing.
3. **Response before durability**: the Cloudflare adapter has no `flush()` (the route calls `db.flush()` only `if (typeof db.flush === "function")`), so the import returned 200 while the D1 writes were still running under `ctx.waitUntil`, which Workers cancel about 30 s after the response. A 5,000-row restore is about 100 sequential batches of 50.
4. Persist failures were only `console.warn`ed, so the caller was told "success".

### B0 update: the actual cause, found while fixing

`mergeBackups()` returns a wrapper `{ merged, stats, errors }`. The profile page posted `JSON.stringify({ ...merged, password })`, where `merged` was that wrapper, so every **merge-mode import** (the default mode, and the separate Merge button) sent the top-level keys `merged`, `stats`, `errors`, `password` and **no table**. `importDb` has replace semantics: it deleted settings, providerConnections, providerNodes, proxyPools, apiKeys, combos and four kv scopes, inserted nothing, and the route answered 200 `{ success: true }` (counts all 0). That is exactly the live D1 state (no INSERT statements, 3,634 rows gone, other tables intact). This also happens on the Node build: it is not Cloudflare-specific; on Cloudflare the same payload additionally returns before D1 is written.

Fix (commit below, separate from the docs): `buildMergeImportPayload()` returns the merged tables or throws; `importDb` rejects a payload with no backup table or with wrong shapes (`{}`, `{ password }`, wrapper keys); the Cloudflare adapter now has `flush()` (the route already awaits `db.flush()` when present), sends upserts before deletes and reports failed writes; `tests/unit/db-import-safety.test.js` (10 tests, 8 fail on the old code). Full vitest: 502 files passed, 24 skipped, only the two known load failures.
Replace mode (raw backup file as the body) was not affected by this bug.

## B1 backup before wiping

`wrangler d1 export router-db --remote` -> `.agents/backups/router-db-before-wipe-20261030-135218.sql` (809,034 bytes, 565 `INSERT`s: _meta 1, auditLog 158, kv 3, requestDetails 350, usageDaily 6, usageHistory 47). This is the state the user's failed import left behind (no providers). The dir is git-ignored.

## B2 load file

Built with the app's own code, not a hand-rolled key scheme: a throw-away vitest drove the real `importDb` over the real `cloudflareMemoryAdapter` backed by a scratch SQLite stand-in for D1 (`flush()` awaited), then dumped every persisted `ninerouter_rows` row as `INSERT`s (8 files, 400 rows each, 119-387 KB, in `.agents/backups/load-20261030-135302/`, git-ignored).

* Settings: the backup's 42 settings keys, with `password` (and `oidcClientSecret`, absent) taken from the settings row that `router-db` held before the wipe (the imported mihir-router snapshot), i.e. what the route's `preserveProtectedSettings` does. The backup itself has no password.
* Rows generated: settings 1, providerConnections 62, providerNodes 18, apiKeys 7, combos 11, kv 2,972 (modelAliases 1,084 + customModels 1,888), total 3,071. proxyPools / mitmAlias / pricing: 0 in the backup. auditLog / usageHistory / usageDaily / requestDetails / _meta: not generated, they start empty.
* Round trip: a fresh adapter reading the scratch D1 and running `exportDb()` returns exactly the backup's connection ids, node ids, api key ids+keys, combo ids+names, alias map, custom-model composite keys and every settings key except the preserved password; per-connection id/provider/name/credential/isActive tuples equal.
* Dry run with `wrangler d1 execute --local --persist-to <tmp>`: seed from the B1 export (565 rows), `DELETE FROM ninerouter_rows` (0 rows), the 8 files -> 3,071 rows with the counts above.

## B3 empty + load (remote `router-db`, account 1da22c19...)

* `SELECT ... GROUP BY` before: _meta 1, auditLog 158, kv 3, requestDetails 350, usageDaily 6, usageHistory 47 (565).
* `DELETE FROM ninerouter_rows` (565 changes; the table and the database stay), then the 8 load files with `wrangler d1 execute router-db --remote --file ... --yes` (400 statements each, 271 in the last, 3,071 total; no errors).
* After: **apiKeys 7, combos 11, kv 2,972, providerConnections 62, providerNodes 18, settings 1** (3,071 rows) = the backup's counts. A key-by-key comparison of the remote rows (table, row key, data length) against the scratch build: 3,071 = 3,071, 0 only on either side. Of the 1,209 row keys that existed only in the old snapshot, 0 remain.
* Note: this left the live isolates holding the earlier (wiped/old) in-memory state; B4 redeploys to evict them.
