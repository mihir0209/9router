# Upstream PR review — `decolua/9router`, as of 2026-10-03

Researched with 5 parallel agents (providers / translator-streaming / auth-DB /
dashboard-CLI / merged-audit). GitHub REST API was rate-limited for unauthenticated
calls, so enumeration used PR HTML pages, the search index, and `git ls-remote` /
merge-base checks against `upstream/master` tip `a99cf572` (v0.5.95). No PR numbers
are invented below; items an agent could not verify are marked as such.

Headline results:

- **Zero missed merges.** The audit agent confirms everything merged upstream since
  2026-09-01 is an ancestor of our v0.5.95 sync point. Nothing to backport.
- **~40 open PRs worth taking**, almost all small, tested, single-concern fixes.
  Listed by area below with file scope.
- A shorter WATCH list of larger ideas needing adaptation (mostly Workers
  incompatibility or overlap with our fork's own fixes).

## TAKE — small, safe, cherry-pickable

### Providers / catalog / capabilities / pricing

- #4546 — model seeds for MiMo + Kiro MITM (`registry`, test). Additive seed only.
- #4515 — nvidia NIM list 7 → 33 chat models + 16-test file. Static seed, Workers-safe.
- #4514 — capability patterns for `qwen3.8*` (1M ctx) and `longcat-2.5*` (~650k).
  Ordered before generic fallbacks. Tiny.
- #4474 — route bare `gpt-5.6-luna` slug to Copilot. Same family as the merged
  bare-slug→codex routing. Likely one registry/alias file.
- #4064 — stop o-series globs capturing Cline's Solar Pro 4. Narrow glob-order fix.
- #4048 — OpenAI reasoning models cannot disable thinking. Caps-flag correction.
- #3995 — register renamed `deepseek-flash` id + scope adapter pools.
- #4329 — resolve OpenRouter tilde `latest` aliases before capability lookup.
- #4371 — delete `globalThis` slot on `setCatalogSource(null)`.
- #3942 — opencode-go DeepSeek V4.1 Flash (take only if the model is wanted).

### Translator / streaming / usage extraction

- #4547 — map Chat Completions `response_format` → Responses `text.format`
  (`translator/request/openai-responses.js` + test). Fixes structured-output loss.
- #4536 — preserve `reasoning_tokens` through pivots and the JSON converter
  (`streamToJsonConverter.js`, responses req/res, `usageTracking.js`).
- #4533 — honor explicit `max_tokens`/`max_completion_tokens` caps
  (`translator/formats/maxTokens.js`, `openai-to-claude.js` + test).
- #4545 — clamp tool names to the 64-char API limit (`appConstants.js`, opencode
  executors + test).
- #4523 — surface CommandCode cache tokens without inflating `prompt_tokens`
  (`translator/concerns/usage.js` + tests).
- #4518 — forward `input_image` tool outputs as image blocks, not stringified
  base64 (`responsesApi.js`, `openai-responses.js` + test).
- #4499 — emit `status` on translated output items (`openai-responses.js` + test).
- #4481 — keep Claude prompt-cache split in OpenAI-format usage
  (`nonStreamingHandler.js`, `usageTracking.js` + test).
- #4477 — preserve in-band stream errors across formats (4 response translators
  + test). Prevents silently swallowed errors.
- #4457 — keep reasoning chunks arriving as `delta.reasoning` /
  `delta.reasoning_details` (`stream.js`, `streamHelpers.js` + test).
- #4449 — support `input_tokens_details` / `output_tokens_details` in usage
  normalization (`usageTracking.js`). Two-line-class change.
- #4412 — sum usage parts when Gemini omits `totalTokenCount`
  (`nonStreamingHandler.js` + test).
- #4403 — keep every system message, not just the last (`openai-to-gemini.js`
  + tests). Stops system-prompt loss.
- #4397 — pair each tool result with its own call, not a global id map
  (`openai-to-gemini.js` + tests). Check overlap with #4274 before taking both.
- #4383 — preserve messages with unknown block types (`container_upload`)
  (`formats/claude.js`, `claude-to-openai.js` + test).
- #4376 — forward SSE comment heartbeats in translate mode (`stream.js` + test).
  Complementary to our `[DONE]` fix, not conflicting.
- #4143 — stop sending vendor-native thinking fields to OpenAI-compatible
  resellers (`paramSupport.js`, `reasoning.js`, registries + tests).
- #4063 — stop emitting literal `<think>` tags on Claude → OpenAI
  (`claude-to-openai.js` + snapshots).
- #3936 — repair trailing assistant prefill instead of dropping it
  (`assistantPrefillPolicy.js`, `openai-to-claude.js` + tests).
- #3968 — IMAGE/DOCUMENT blocks for the Antigravity Claude envelope
  (`claude-to-openai.js`, `openai-to-gemini.js`).
- #3772 — placeholder for binary `tool_result` blobs instead of raw base64
  (`claude-to-openai.js`, `openai-responses.js` + tests).
- #4464 — always emit `tool_result` `is_error` for the Zed Anthropic wire
  (`openai-to-claude.js` + tests).
- #4349 — stop treating parameter names as JSON Schema keywords
  (`formats/gemini.js` + test).
- #4382 — rename tool parameter named `properties` to avoid Gemini 400
  (`formats/gemini.js` + test).
- #2178 — forward `input_type` to the provider request body
  (`embeddingsCore.js`, `embeddingProviders/openai.js`).

### Auth / fallback / DB / usage infra

- #4289 — reject documented example `INITIAL_PASSWORD`s at login
  (`lib/auth/initialPassword.js` + test). No behavior change for real creds.
- #4330 — build OAuth `redirect_uri` from the public base URL
  (`lib/oauth/redirectUri.js`, OAuthModal + test). Fixes OAuth behind proxies.
- #4389 — preserve active `modelLock_*` entries when a connection is marked
  active (`connectionsRepo.js` + test).
- #4439 — rotate account on per-key credit exhaustion signalled via 400
  (`errorConfig.js` + test). Complements our fallback fix.
- #4484 — make the client context margin configurable (`USAGE_BUFFER_TOKENS`,
  `usageTracking.js` + test/docs).
- #4350 (merged 2026-09-26) — POST `/api/providers` O(1) + refuse silent key
  overwrite. Verify present; pull if predated.
- #3830 (merged 2026-09-09) — clear stale locks after validation. Verify present.

### Dashboard / CLI (no server impact)

- #4522 — CLI: attach server lifecycle events before entering tray mode
  (`cli/cli.js`, pure reorder).
- #4519 — CLI `connect` takes models from server; add `show`, `--save`, Pi
  support (builds on already-merged `connect` command).
- #4512 — expose Codex credit balance + Luna Reserve rows (quota display only).
- #3368 — stop the hard-coded heap cap overriding the operator
  (`cli/cli.js`, `nodeFlags.js` + test).
- #1919 — collapse quota rows, sort by remaining desc
  (`ProviderLimits/QuotaTable.js`). Rebase onto current table first.

## WATCH — good ideas needing adaptation or triage

- #4475 (azure Responses API per connection), #4469 (CodeBuddy auth-token mode),
  #4321 (custom headers for nodes/connections), #4492 (test models with selected
  connection) — overlap our fork's Azure scoping / providerOverrides /
  auth areas; extract hunks, don't cherry-pick whole.
- #4524 (codex `gpt-6.1-sol` 872000) — direct value dispute with our 272000 cap;
  compare live-probe evidence before touching.
- #4534 (default omitted `stream` to JSON), #4513 (echo model in `message_start`),
  #4466 (forced SSE→JSON for Claude), #4352/#4332 (stream failure accounting),
  #4298 (non-streaming normalization for `/v1/responses`), #4486 (Gemini
  truncated streams), #4404 (native reasoning-effort shape), #4396 (tool `strict`
  semantics), #4386 (custom tool calls), #4222 (assistant-turn merge), #4442
  (tool-schema coercion), #4274 (tool-id disambiguation, overlaps #4397),
  #3878 (six-defect translator roll-up), #3117 (Claude→Antigravity images, huge
  stack — take image hunks only): all translator/streaming-adjacent, verify
  against our `[DONE]` fix and thinking conventions first.
- #4286 (GHSA reconciliations, 56KB, touches our customized auth/session),
  #4393 (500-series cooldown 30s→5s — interacts with our coordinator breaker;
  decide deliberately), #4340/#4381/#4364/#4372 (combo fallback variants —
  verify against our `checkFallbackError` fix), #4348 (Gemini key parking —
  overlaps our circuit breaker), #4365 (raw-frame logging), #4526 (decode-speed
  stats), #4458/#4538/#4541 (API-key limits/ACL/policy models — competing with
  our keyContext/BYOK; reconcile the three first), #4468 (Devin provider —
  loopback+protobuf, Workers-hostile), #4157 (Bedrock SSO — Node-SDK-shaped),
  #4068/#3751 (pricing edits — diff first), #4067 (Copilot auto-select),
  #4066/#4525 (codex fast-mode flags), #4034/#3909 (combo caps merges),
  #3880 (omnibus catalog fix — per-hunk review), #4292 (codex catalog scoping),
  #4473 (codex CLI identity bump — verify open, then likely TAKE), #3768
  (Gemini swap + combo weights — split hunks), #3780 (Muse Spark sessions),
  #4492 (dup), #4521 (MITM bundle — UI half only), #2588 (retry engine, huge),
  #2575 (dashboard updater — desktop-only), #1129/#1277 (stale UI bundles),
  #1247 (May-vintage guard hardening — audit, don't blind-apply).
- #3423 (Qwen3.8/Muse caps) — stale vs #4514 + merged Muse provider.
- #4473 — content per excerpt only (identity 0.159.0); verify state first.

## SKIP — already present, superseded, stale, or Workers-hostile

Already in our tree (don't re-take): #4524-value dispute aside, #4462/#4437/#4423
(sentinel territory — ours fixed), #3974/#4341 (custom-model caps —
providerOverrides covers), #3757/#3124 (Muse Spark PRs — provider merged in
v0.5.95), #3423 (stale), #2439 (stale Grok list), #3751-context, #4378 (4xx
opt-out — our `checkFallbackError` fix covers the same ground; compare
status sets only), #4318/#4388 (disabled-model routing dupes), #4324 (CF Pages
PR — our CF scripts differ deliberately), #3313 (DNS-per-connect SSRF —
Workers-incompatible by design; #3714 covers), #1296 (user-script sandbox on
Edge — severity risk), #1530 (embeddings combos — different slice),
#4455/#4521 (un-cherry-pickable bundles), #4408/#4415/#4426 (test-only),
#4350-backend/#3830 if verified present, #1293/#3370/#2185/#769/#1063/#602/
#1080/#2354/#416 (merged pre-Sept or closed).

## Method limits

Unauthenticated REST was rate-limited (403), so per-PR file lists come from PR
HTML bodies and the patch headers agents could fetch; keyword pagination
(counts like translator-246/streaming-160) was not exhaustively enumerated, so
treat the TAKE list as high-value sample, not census. The merged-audit slice
used `git log upstream/master --since=2026-09-01` with merge-base checks, which
is complete: **no merged PR since 2026-09-01 is missing from our v0.5.95 sync**.
Upstream tip is the v0.5.95 bump itself — there is nothing newer upstream than
what we hold (`a99cf572`).
