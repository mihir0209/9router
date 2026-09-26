# Live-provider E2E suites

These suites make **real** calls against real provider credentials and write to the
real `~/.9router` database. They are skipped unless `RUN_REAL=1` is set, so a
plain `npx vitest run` never touches the network or your accounts.

## Running

```bash
RUN_REAL=1 npx vitest run -c vitest.real.config.js
```

Use `vitest.real.config.js` rather than the default config. It sets
`fileParallelism: false`, which matters: every suite here shares one SQLite file
(token refresh, connection rotation, usage rows), and Vitest's parallel file
execution made concurrent writers fail with `database is locked` — an artefact of
the harness, not a product defect.

### Selecting providers

| Variable | Meaning |
|---|---|
| `REAL_PROVIDERS` | Comma-separated allow-list. Omit to auto-discover from active connections. |
| `REAL_SKIP_PROVIDERS` | Comma-separated deny-list. |
| `REAL_INCLUDE_COMPAT` | `1`/`true`/`*` to include `openai-compatible-*` nodes. |

Only providers with a currently-credentialed connection are discovered, and
`getProviderCredentials` is what decides that — an account marked unavailable is
skipped silently rather than reported.

## Interpreting failures

Not everything that fails here is a router bug, and the distinction is the whole
point of these suites. Three classes of failure are **not** actionable:

1. **Credential skips** — HTTP 401/402/403/429 upstream is reported as a skip
   (`isCredentialSkip`), not a failure.
2. **Model compliance** — a live LLM may not follow an instruction literally.
   Assertions here therefore check router behaviour (is a tool call emitted,
   does a stream terminate, does the tool result reach the model) and never
   demand a specific magic string back. Earlier revisions asserted exact tokens
   like `NO_TOOL` and `TERMINATED`, and compared two independent completions for
   string equality; all three failed at random while the router was demonstrably
   correct, and the equality check compared two separate model calls, which proves
   nothing.
3. **Empty completions** — a reasoning model may finish on an empty completion,
   so a terminal event with no text is valid.

To confirm whether a failure is the router or the provider, dump what actually
went over the wire: `invokeChat` returns `raw` (the full response text) alongside
`json`, and `stream` (the reassembled SSE). A forced `tool_choice` should produce
a standard `choices[0].message.tool_calls[0].function` object; if that is present
and well-formed, the translation layer did its job.

## Cost

These calls are billed to your accounts. Scope them with `REAL_PROVIDERS` when
you only care about one provider.
