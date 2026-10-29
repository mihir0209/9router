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
