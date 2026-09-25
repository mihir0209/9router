# Real end-to-end tests

These tests exercise the local 9Router database, credential selection, provider translators, and live upstream providers. They are inert during ordinary test runs and are skipped unless `RUN_REAL=1` is set.

## Default check

From the repository root:

```bash
npx vitest run --config tests/vitest.config.js "tests/e2e/real"
```

The default command performs collection and skips all real suites. It does not make provider calls. The existing test dependencies must first be installed:

```bash
npm install
(cd tests && npm install)
```

## Live runs

Use an explicit provider allow-list for a bounded run:

```bash
RUN_REAL=1 REAL_PROVIDERS=claude,gemini npx vitest run --config tests/vitest.config.js "tests/e2e/real"
```

`REAL_PROVIDERS` limits the active database providers. Provider discovery reads active connections from the real local SQLite database through the same credential path as the application. Models are selected from the provider catalog, and capability checks use `getCapabilitiesForModel`.

`openai-compatible-*` providers are excluded by default because they can introduce a large or unexpected number of upstream calls. Include them explicitly with `REAL_INCLUDE_COMPAT=1`, or name a specific compatible provider in `REAL_INCLUDE_COMPAT`.

Additional controls:

- `REAL_SKIP_PROVIDERS` removes providers from the discovered set.
- `REAL_INCLUDE_COMPAT` accepts `1`, `true`, `*`, or a comma-separated provider list.
- `E2E_FORCE_REFRESH=1` is required in addition to `RUN_REAL=1` for the OAuth forced-refresh test. Without both values, OAuth tests only inspect local inventory and make no refresh call.
- `DATA_DIR` selects the local 9Router data directory; otherwise `~/.9router` is used.

The suites use one eligible model per provider when possible and keep prompts and output limits small. A provider response with HTTP 401, 402, 403, or 429 is treated as a credential or quota skip. Other upstream errors, including 400 and 500, fail the test instead of being broadly hidden.

## Suites

- `vision-structured.real.test.js` checks base64 image input, semantic image evidence, and `json_schema` output in streaming and non-streaming modes.
- `tools-streaming.real.test.js` checks forced calls, tool-result round trips, `tool_choice: none`, terminal SSE events, stream/non-stream equivalence, and usage frames.
- `oauth-refresh.real.test.js` inventories OAuth connections read-only by default and runs a live usability check only behind the double refresh gate.
- `combo-fallback.real.test.js` calls `handleComboChat` directly. It does not create or update combo records and covers healthy fallback, missing credentials, all-members failure, and capability reordering.

Live runs consume provider quota and may send the tiny image and test prompts to the selected upstream services. Start with `REAL_PROVIDERS` containing one provider and review the provider's billing and quota limits before expanding the run.
