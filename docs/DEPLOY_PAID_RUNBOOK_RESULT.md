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

## Status

Stopped at P2. Not done: D1 create/import, secrets, deploy, smoke, soak, Cloudflare analytics, vitest run (nothing in `src/` changed; only the build script, config, npm scripts and smoke helper).

## Commands to resume once the login has write access to the paid account

```bash
export CLOUDFLARE_ACCOUNT_ID=1da22c19548115accf1b98ab7c562b2c
npx wrangler d1 create router-db                       # copy database_id into wrangler.router.toml
npx wrangler d1 execute router-db --remote --file .agents/backups/mihir-router-db-pre-deploy.sql --yes
npx wrangler d1 execute router-db --remote --command "SELECT table_name, COUNT(*) n FROM ninerouter_rows GROUP BY 1"
# secrets BEFORE the first deploy (wrangler offers to create the Worker `router` for them): JWT_SECRET, API_KEY_SECRET, MACHINE_ID_SALT,
# ADMIN_API_TOKEN, INITIAL_PASSWORD from .env.deploy, CRON_SECRET from .agents/secrets/router-cron-secret.txt (stdin or `secret bulk`)
npm run deploy:router                                   # build once, then wrangler deploy --config wrangler.router.toml
LIVE_STATE_CONFIG=wrangler.router.toml scripts/smoke/live-state.sh https://router.genzzz.workers.dev <version prefix>
```
