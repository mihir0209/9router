
# Splitting the dashboard from the gateway (design note, not implemented)

Status: feasibility note. Nothing here is built. Numbers marked *measured* come from the commands in
[How the numbers were obtained](#how-the-numbers-were-obtained); numbers marked *estimate* are not measured.

## Why

One Worker serves both the `/v1/*` gateway and the whole Next.js dashboard. The gateway is the hot,
latency-sensitive, cost-sensitive part; the dashboard is used by one operator. Today every cold isolate
parses and evaluates the Next server runtime and every dashboard page to serve a chat request.

| | Raw | Gzip | Source |
|---|---|---|---|
| Current Worker (`wrangler deploy --dry-run`, after the bundle work in this branch) | 11.3 MiB | 2.75 MiB | *measured* |
| Same Worker before that work | 20.2 MiB | 3.96 MiB | *measured* |
| Gateway-only bundle (chat, embeddings, image, stt, tts handlers + translators + executors, no Next, no dashboard) | 1.71 MiB | 0.50 MiB | *measured* with esbuild, minified |

The gateway-only figure covers five of the `/v1` route families, not all of them; the missing ones
(`/v1/models`, `/v1/messages`, `/v1/responses`, search, fetch, video, systemone, Gemini-native) are mostly
thin wrappers around the same engine, so the full gateway should land well below 1 MiB gzip (*estimate*).
That is under the Free plan's 3 MB compressed limit with a wide margin, and a fraction of the Paid 10 MB.

Expected effects (*estimates*, to be confirmed with `wrangler dev` / a canary):

* **Cold start**: parse and top-level evaluation of about 1.7 MiB of JavaScript instead of 11 MiB. The
  Next server runtime alone is 6.7 MB of the 23 MB of webpack module source (*measured*, `NINEROUTER_WEBPACK_STATS`).
* **Memory per isolate**: the 128 MB isolate limit applies to both, but the baseline heap of the gateway
  drops by the size of the Next runtime and the dashboard module graph, leaving more for concurrent requests.
* **Blast radius**: a dashboard bug, a heavy dashboard page, or a scanner hammering `/dashboard` can no
  longer exhaust the isolates that serve `/v1`.
* **CPU limit**: the Free plan's 10 ms per request is dominated by cold-start parse today; a 1.7 MiB gateway
  has a realistic chance on Free, the current 11 MiB Worker does not.

## Target layout

```
                       ┌────────────────────────┐
  /v1/*, /v1beta/*  →  │ gateway Worker          │  plain fetch handler, no Next
  /api/v1/*            │  src/sse + open-sse     │
                       │  D1, GatewayCoordinator │
                       └────────────────────────┘
                       ┌────────────────────────┐
  everything else   →  │ dashboard Worker        │  Next.js via OpenNext (today's build)
  (/dashboard, /api/*) │  static assets          │  minus the gateway routes
                       └────────────────────────┘
```

Routing: a custom domain with two routes (`example.com/v1/*` to the gateway, the rest to the dashboard), or
a thin router Worker with two service bindings. The existing `WORKER_SELF_REFERENCE` binding shows the
pattern.

## Steps

1. **Extract a framework-free entry.** `src/sse/handlers/*.js` already take a `Request` and return a
   `Response`. Write `src/cloudflare/gateway.js` with `export default { fetch }` that maps `/v1/chat/completions`,
   `/v1/embeddings`, ... to those handlers, adds CORS and OPTIONS, and re-uses the guard logic
   (`scripts/build-cloudflare.mjs` already generates it as plain JS: API-key presence check, body size 413,
   `x-9r-real-ip` rewrite). The Next rewrites in `next.config.mjs` (`/v1/:path*` to `/api/v1/:path*`,
   `/codex/*`, `/responses`, `/systemone`, `/v1beta`) become a path table.
2. **Bundle it without Next.** `esbuild --bundle --platform=node` with the aliases already used for the
   Cloudflare build (`@` to `src`, `open-sse`, the `driver.cloudflare.js` swap, the stubs in
   `src/cloudflare/stubs/`), node builtins external under `nodejs_compat`. This is what produced the
   1.7 MiB figure above.
3. **Second `wrangler.toml`** (`wrangler.gateway.toml`): same D1 database, same `GatewayCoordinator` Durable
   Object (declare the class in exactly one Worker and bind to it from the other with `script_name`), same vars.
   Cron stays on the dashboard Worker or moves to the gateway, whichever owns the buffers that need flushing
   (the analytics buffer lives in the isolate that served the request, so it should be the gateway).
4. **Route table / domain**, then strip the gateway routes from the dashboard build (they stay harmless but
   cost bundle bytes: `src/app/api/v1/**`).
5. **Session and key plumbing**: the gateway never needs the dashboard cookie. API-key validation, ACL, pins,
   budgets and rate limits already live in `src/sse/services` and `src/lib`, not in Next.
6. **Canary**: deploy the gateway Worker next to the old one on a second hostname, replay traffic with
   `scripts/bench/` against both, compare error rate, p99 and `1102` counts, then switch the route.

## Risks and open questions

* **Config drift**: two Workers means two `[vars]`, two sets of secrets. Mitigation: one generated `[vars]`
  block, `wrangler secret bulk` for both.
* **Single source of truth for `process.env`**: the Next build copies `env` into `process.env` through
  OpenNext's `init`; the gateway entry must do the same (the guard's `__9routerSyncEnv` already does it
  once per isolate).
* **Module-level state shared with the dashboard**: model catalogue sync (`src/lib/modelCatalog/sync.js`),
  the tunnel and MITM helpers and `src/instrumentation.js` hooks are started by Next. None of them is needed on
  Workers, but check each import chain from the handlers (`@/lib/localDb` re-exports a lot) so the esbuild
  graph stays small; `NINEROUTER_WEBPACK_STATS` and the esbuild metafile show regressions.
* **Dashboard features that call `/v1` on the same origin** (playground, model test, `test-models`) keep
  working with path routing on one hostname. With two hostnames they need an absolute gateway URL or the
  service binding.
* **Durable Object ownership**: moving the class between Workers needs a migration; do not move it. Keep it
  in the Worker that owns it today and bind from the gateway.
* **`/v1/models` fingerprint cache and `/api/cron/flush`** are per isolate; both keep working, but the flush
  must reach the gateway isolates, not the dashboard ones.
* **Not measured**: real cold-start time on workerd for either bundle. Do this with `wrangler dev --local`
  and a canary before relying on the estimates above.

## How the numbers were obtained

* Worker sizes: `npm run build:cloudflare`, then `npx wrangler deploy --dry-run --outdir /tmp/out`
  (reports "Total Upload"; nothing is deployed) and `gzip -9` of `/tmp/out/worker.js`.
* Per-module breakdown of the Next server bundle: `NINEROUTER_WEBPACK_STATS=/tmp/stats.json npm run build:cloudflare`
  writes `/tmp/stats.json.server`, a flat list of `{name, size}` for every module of the server compilation.
* Gateway-only bundle: esbuild with entry points `src/sse/handlers/{chat,embeddings,imageGeneration,stt,tts}.js`,
  `--minify --platform=node`, aliases and externals as in step 2, `NINEROUTER_RUNTIME` defined as `"cloudflare"`.
