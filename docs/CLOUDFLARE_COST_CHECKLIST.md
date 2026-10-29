
# Cloudflare cost and memory checklist

Scope: the Worker deployed with `npm run build:cloudflare` + `wrangler deploy`. Limits used below
(from Cloudflare's published Workers limits; re-check them before relying on them):

| | Free | Paid |
|---|---|---|
| CPU time per request | 10 ms | 30 s default, configurable (`limits.cpu_ms`) |
| Worker size, compressed | 3 MB | 10 MB |
| Memory per isolate | 128 MB, shared by concurrent requests | 128 MB, shared by concurrent requests |
| Simultaneous outbound connections per request | 6 | 6 |

Error 1102 means the Worker exceeded its CPU or memory limit; 1101 means the Worker threw.

## Expert checklist mapped to this repo

| Item | Status | Where / why |
|---|---|---|
| Static assets from the CDN, long cache | **done now** | `scripts/build-cloudflare.mjs` writes `.open-next/assets/_headers`: `/_next/static/*` is `immutable`, 1 year (content-hashed names); icons/providers 1 day; i18n 1 hour; `sw.js` revalidates. Assets are served by Workers Assets before the Worker runs. |
| Stream instead of buffering | **done now** (streams) / **bounded** (non-stream) | Streaming chat was already pass-through. Added: request body caps (413 by `Content-Length`, or by counting a chunked body, before parsing), a cap on text kept for request details, a byte cap on the combo empty-failover peek, bounded reads of upstream error bodies, 502 above a size on upstream bodies that must be read whole (image, embeddings, non-stream, SSE-to-JSON). The worker guard no longer `clone()`s the request body (the tee buffered the unread branch). |
| Outbound concurrency limits | **done now** | Dashboard fan-outs (proxy health check and scrape validation, geo refresh, model tests, per-key rate-limit peeks) are capped at 5 per request on Cloudflare (`NINEROUTER_OUTBOUND_CONCURRENCY` to change). Node keeps its configured concurrency. The gateway itself makes one upstream call per attempt. |
| D1 consolidation / fewer D1 operations | **done earlier** + **done now** | The "extreme" profile (`docs/CLOUDFLARE_EXTREME_PROFILE.md`). Now also: a cold budget-key read loads one key's in-window rows instead of the whole `usageHistory` table (4,082 to 81 rows read in the test dataset). |
| Bundle pruning | **done now** | `node-forge`, `@node-saml/node-saml` (+xpath, xmldom, xml-crypto), `@aws-sdk/credential-providers`, `undici`, `socks-proxy-agent`, `recharts`, `@xyflow/react` aliased to stubs in the Cloudflare build only (`src/cloudflare/stubs/`); `wrangler.toml` `minify = true`. See numbers below. |
| Log filtering / sampling | **done now** | `LOG_LEVEL` defaults to WARN on Cloudflare; Workers Logs sampling 0.25/0.1 to 0.05/0.01; the console-capture patch is not installed on Workers. |
| Billing alerts | **user action** | Dashboard, see below. |
| KV batching | not applicable | KV is not used. |
| R2, Queues | not applicable | Not used. The OpenNext R2 cache bucket in `wrangler.toml` is commented out. |
| WASM | not applicable | The gateway has no CPU-bound kernel that WASM would speed up; its cost is I/O and JSON handling. `sql.js` (a WASM SQLite) is not used on Workers (D1 is). |
| Cache API for responses | not applicable | Gateway responses are per API key and per request body (private, non-repeatable), so a shared cache would be wrong. The one cacheable response, `GET /v1/models`, already has an in-isolate fingerprint cache and still needs the key check. |
| Hyperdrive | not applicable | Supported by Workers for external Postgres but not bound here. D1 is the durable store on Cloudflare; `DATABASE_URL` Postgres is skipped on this runtime. |
| `GET /api/health` cache header | skipped | It is public and used as a liveness probe; a cache header would let a CDN or monitor see stale health. The response is `{"ok":true}` and already costs no D1 operation. |

## Request and response limits (new)

| Env | Cloudflare default | Elsewhere | Applies to |
|---|---|---|---|
| `NINEROUTER_MAX_BODY_MB` | 20 | off | JSON request bodies (chat, embeddings, image, tts, search, fetch, ...) |
| `NINEROUTER_MAX_UPLOAD_MB` | 32 | off | multipart / binary uploads (audio, video) |
| `NINEROUTER_MAX_RESPONSE_MB` | 32 | off | upstream bodies read whole (image, embeddings, non-stream, SSE-to-JSON) |
| `NINEROUTER_STREAM_CAPTURE_MAX_BYTES` | 65536 | unbounded | text kept per field (content, thinking) for request details |
| `NINEROUTER_EMPTY_PEEK_MAX_BYTES` | 262144 (all runtimes) | same | bytes the combo empty-failover holds before it commits |
| `NINEROUTER_OUTBOUND_CONCURRENCY` | 5 | caller's value | dashboard fan-outs |
| `LOG_LEVEL` | `warn` | `info` | `src/sse/utils/logger.js` and per-request console lines |

`0` disables a limit. The worker guard answers 413 from `Content-Length` before any D1 or Next work; chunked
bodies are counted by the handlers while they read. `proxyClientMaxBodySize` is 32 MB in the Cloudflare
build (it was 128 MB, the size of the isolate): Next buffers every body that passes the middleware up to it.

Request details (`requestDetails` rows) are not written on Cloudflare unless `lowWriteMode` is explicitly set
to `false` in settings: `saveRequestDetail` returns early in low-write mode, which is the Cloudflare default.

## Measured

Environment: Node 24 on a desktop, scrubbed copy of the developer database, local mock upstream
(`scripts/bench/memory-check.mjs`). Peak is the process's resident-set high-water mark (VmHWM).

| Scenario | Before | After, defaults (Node) | After, Cloudflare limits (`MAX_BODY_MB=20`, `STREAM_CAPTURE=65536`) |
|---|---|---|---|
| 50 MB base64-in-JSON chat body | 625 MB peak, 200 | 620 MB, 200 (unchanged by design) | **213 MB peak, 413 in 0.3 s** |
| 200 MB SSE response, streamed | 498 MB peak | 518 MB | **232 MB** |
| same two, `--max-old-space-size=128` | server dies (both) | server dies (both) | **survives, 208 / 255 MB RSS** |

Worker upload (`wrangler deploy --dry-run`, nothing deployed):

| | Raw | Gzip |
|---|---|---|
| Before | 20,680 KiB | 4,053 KiB (over the Free 3 MB limit) |
| After | 11,550 KiB | 2,815 KiB |

`.open-next/server-functions/default/handler.mjs`: 14,007,656 B raw / 3,320,922 B gzip before; 11,703,008 B / 2,662,864 B after.
The minified Worker was started under `wrangler dev --local` (workerd): health, 401 without a key, 413 above the
limit, the app's own 401 for a bad key, dashboard redirect and the immutable asset header all behave.

Not verified: real Cloudflare 1102 behaviour, real D1 `rows_read` (the counting D1 models returned rows; the new
budget select is index-driven in SQLite's planner, D1 should plan it the same way), and Free-plan CPU time.

## Before deploying to the paid account

In the Cloudflare dashboard (nothing below is done by this repo):

1. **Plan**: confirm the account is on Workers Paid. On Free the 10 ms CPU limit and the 3 MB compressed size
   limit apply; the current upload is 2.75 MiB compressed, so it fits Free's size limit but not necessarily its CPU limit.
2. **Billing notification**: Notifications, add a usage/billing alert for Workers (and D1, Durable Objects) at a
   threshold you are comfortable with. (The exact notification name differs between dashboard versions: verify.)
3. **Scanner traffic**: every request, including a scanner's 401, is a Worker invocation. Put the Worker on a
   custom domain and add a WAF rate-limiting rule plus Bot Fight Mode for that zone. Zone rules do not apply to
   `workers.dev` hostnames (believed; verify), so also set `workers_dev = false` once the custom domain works.
4. **CPU limit**: add `[limits] cpu_ms = ...` to `wrangler.toml` if you want a ceiling lower than the 30 s
   default, so a runaway request is cut off instead of billed. Not set in this repo.
5. **Placement**: decide on `[placement] mode = "smart"` (not set). The Worker talks to D1 and to the Durable
   Object on most requests and to remote LLM APIs; placement near D1 trades client latency for DB latency. Unmeasured here.
6. **Logs**: Workers Logs sampling is 5% / traces 1% in `wrangler.toml`. Raise it temporarily when debugging.
   Errors in unsampled invocations are still visible in `wrangler tail`.
7. **Secrets** before the first request: `JWT_SECRET`, `INITIAL_PASSWORD` (not `123456`), `API_KEY_SECRET`,
   `MACHINE_ID_SALT`, `CRON_SECRET` (without it `/api/cron/flush` is public: a cheap but unauthenticated Worker invocation).
8. **Cron**: `*/10 * * * *` calls the Worker through the `WORKER_SELF_REFERENCE` service binding (no hard-coded
   hostname). It is still one extra invocation per tick that lands on an arbitrary isolate.
9. **Run it once locally**: `npx wrangler dev --local`, then check `/api/health`, a 401 without a key and a 413
   with a body above the limit before `wrangler deploy`.
