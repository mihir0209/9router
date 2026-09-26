# Issue: test suite writes fixture accounts into the real `~/.9router` production database (half-fixed; two gaps remain)

> **Fill-in notes for the maintainer/reporter:** everything below is verified against
> `decolua/9router@master` and a real local `~/.9router/db/data.sqlite`. Adjust the
> `master`-relative line numbers if the file has moved. Do **not** include the contents
> of your own `data.sqlite`; the aggregate counts below are enough to reproduce.

## Summary

Running the unit test suite inserts fixture provider connections into the developer's
**real** `~/.9router/db/data.sqlite`. The phantom accounts then appear in the running
dashboard's account list, and accumulate by ~6 rows per `npx vitest run`.

On my machine this reached **310 phantom connections** (258 `zed` + 52 `kimchi-nope`)
over ~52 suite runs, all of them visibly fake (`zed-live-m1-1758…@example.com`,
`Account 5`, provider `kimchi-nope`).

The maintainer already landed a partial fix for one of the two files
([`975f28a5`](https://github.com/decolua/9router/commit/975f28a57c059b0189d2d898a1055b09f6ea5960),
2026-09-26, *"test(zed): isolate zed-live-models suite DB via temp DATA_DIR"*, pushed
directly to `master` with no PR). **Two gaps remain, and no issue or PR tracks them:**

| # | Gap | Status |
|---|---|---|
| 1 | `tests/unit/zed-native-auth.test.js` still writes to the real DB | **open** — the source of the `Account N` rows |
| 2 | `tests/vitest.config.js` has no `env` / `setupFiles` / `globalSetup`, so the fix is per-file and can be forgotten | **open** — any new test that calls `createProviderConnection` reintroduces this |

## Root cause

`DATA_DIR` is read once, at module load, and silently falls back to `~/.9router`:

```js
// src/lib/dataDir.js
11:   return path.join(os.homedir(), ".9router");
14: export function getDataDir() {
15:   const configured = process.env.DATA_DIR;
16:   if (!configured) return defaultDir();
...
37: export const DATA_DIR = getDataDir();   // ← one-shot, module-load side effect
```

`tests/vitest.config.js` never sets it. The whole `test` block is:

```js
export default defineConfig({
  test: {
    environment: "node",
    globals: true,
    include: ["**/*.test.js"],
    exclude: ["**/node_modules/**", "**/.claude/**", "**/dist/**"],
    maxConcurrency: 60,
    silent: false,
  },
  // ...
});
```

There is no `env`, no `setupFiles`, no `globalSetup`, and no CI test job
(`.github/workflows/` contains only `docker-publish.yml`, `gitbook-pages.yml`,
`tray-binaries.yml` — `.github/workflows/{ci,test,tests}.yml` all 404). So
`getDataDir()` always takes the `~/.9router` branch during a test run, and nothing
would catch a regression.

`tests/package.json` is `"test": "vitest run --reporter=verbose"` — no `DATA_DIR`.
The repo `.env` has no `DATA_DIR` key (only `.env.example` does, commented). The shell
does not export it.

## Exact creation sites

**`tests/unit/zed-live-models.test.js`** — 258 of the 310 rows:

| line | effect |
|---|---|
| `:85` | `createProviderConnection({` (in `seedZed`, `:84–93`) |
| `:86` | `provider: "zed",` |
| `:89` | `` email: `zed-live-${n}-${Date.now()}@example.com` `` |
| `:102`, `:122`, `:131`, `:143` | `seedZed("m1" \| "m2" \| "m3" \| "m4")` |
| `:161–167` | `createProviderConnection({ provider: "kimchi-nope", …, email: `guard-${Date.now()}@example.com` })` |

`kimchi-nope` is a deliberately non-existent provider id used to assert the models
route does not grow a zed-shaped branch (`:159–173`). `createProviderConnection` does
not validate provider ids, so the row is inserted.

**`tests/unit/zed-native-auth.test.js`** — the 52 `Account N` rows. This file never
calls `createProviderConnection` itself; it drives the real loopback OAuth callback
server, which inserts at `src/lib/oauth/utils/server.js:735–740`. Because the test
stubs `cloud.zed.dev` to HTTP 500 (`:44–51`), `postExchange` yields `email: null`
(`src/lib/oauth/providers/zed.js:47`) and `mapTokens` returns `email: undefined`
(`:55`), so the insert passes no email and `createProviderConnection` falls back to
the auto-generated name:

```js
// src/lib/db/repos/connectionsRepo.js
206:  connectionName = deriveConnectionName(data, data.email || `Account ${all.length + 1}`);
221:  // isActive defaults to true  → 233 phantom accounts were ACTIVE in the dashboard
233:  upsert(db, conn);                          // ← the INSERT
234:  reorderInTx(db, data.provider);           // ← rewrites priority for that provider
```

A polluted row carries the test's own literals, which makes identification exact:

```json
{"accessToken":"decrypted-token-xyz", … ,"userId":"user-123", … }
```

matching `tests/unit/zed-native-auth.test.js:157` and `:159`. `rg -n "zed-live-|guard-|kimchi-nope" tests/ src/`
matches **only** `tests/unit/zed-live-models.test.js` (lines 89, 162, 165), so no
other file can produce these rows.

## Reproduction

1. `sqlite3 ~/.9router/db/data.sqlite "SELECT COUNT(*) FROM providerConnections;"` — note the number.
2. `npx vitest run unit/zed-live-models.test.js unit/zed-native-auth.test.js` (or any full `npx vitest run`).
3. Re-run the count. It grows by 6 (`zed-live-m1..m4` + one `kimchi-nope` + one `Account N`).
4. Observe them in the dashboard's account list.

Measured on my machine: every polluted row has `createdAt` between
`2026-09-23T20:05:44.531Z` and `2026-09-26T07:54:35.967Z`; **every row created before
that window is a genuine account**, which gives a safe cleanup predicate.

```sql
-- identify
SELECT provider, COUNT(*) FROM providerConnections
WHERE provider IN ('zed','kimchi-nope') GROUP BY provider;

-- cleanup (back up ~/.9router/db/data.sqlite first)
BEGIN IMMEDIATE;
DELETE FROM providerConnections WHERE provider IN ('zed','kimchi-nope');
COMMIT;
```

I confirmed 0 dependent rows: `usageHistory` and `requestDetails` (the only tables
carrying `connectionId`) have no rows referencing these connections, and
`providerConnections.priority` gaps are per-provider so no resequencing is needed.

## Why the existing fix is not sufficient

`975f28a5` isolates `zed-live-models.test.js` correctly, via the pattern already used
by `compatible-provider-connections.test.js`:

```js
// Self-isolating: DATA_DIR points at a temp dir so seeding never touches ~/.9router.
const originalDataDir = process.env.DATA_DIR;
beforeAll(async () => {
  process.env.DATA_DIR = fs.mkdtempSync(path.join(os.tmpdir(), "9router-zed-live-"));
  vi.resetModules();
  ({ GET } = await import("@/app/api/providers/[id]/models/route.js"));
  ({ createProviderConnection } = await import("@/models/index.js"));
});
```

But `tests/unit/zed-native-auth.test.js` is still comment-only on this. Its entire
`DATA_DIR` handling is line 2:

```js
// RUN WITH AN ISOLATED DB:  DATA_DIR=$(mktemp -d) npx vitest run unit/zed-native-auth.test.js
```

A comment is not enforcement, and the documented command in both `CLAUDE.md` and
`tests/README.md` is a bare `npx vitest run` with no `DATA_DIR`. Note this file
statically imports `@/lib/oauth/…` at top level, so setting `DATA_DIR` inside the file
body would be too late anyway — it needs `vi.hoisted()` or a `beforeAll` +
dynamic import, as above.

## Suggested fix

Two layers, because the per-file one is whack-a-mole.

**1. `tests/vitest.config.js` — make it impossible to forget.** A `setupFiles` entry
runs inside each worker *before* the test file's module graph is imported, and with
the default `pool: "forks"` + `isolate: true` each test file gets its own child
process, so `process.pid` yields a unique directory per file:

```js
// tests/setup/data-dir.js
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

// Runs before the test file's module graph is imported. src/lib/dataDir.js:37
// evaluates DATA_DIR at module load, so this must run first.
if (!process.env.DATA_DIR) {
  process.env.DATA_DIR = fs.mkdtempSync(path.join(os.tmpdir(), "9router-test-"));
}
```

```js
// tests/vitest.config.js — inside test: { … }
env: { DATA_DIR: path.join(os.tmpdir(), "9router-vitest") },
setupFiles: ["./setup/data-dir.js"],
```

`test.env` is applied to the worker's `process.env` at fork time, before any module
loads, so it is safe for this purpose.

**⚠️ Blocker to handle first:** `include: ["**/*.test.js"]` also collects the 12
live-provider suites under `*/real/`, which are *supposed* to read the real
`~/.9router/db/data.sqlite` (`tests/translator/AGENTS.md:45`;
`tests/translator/real/all-formats.real.test.js:255–257` falls back to
`os.homedir()/.9router/db/data.sqlite` and `catch { return [] }`). A blanket
config-level `DATA_DIR` would make `RUN_REAL=1` auto-discover **zero** providers and
pass vacuously. So either:
- add `"**/real/**"` to `exclude` in `tests/vitest.config.js`, and
- give the real suites their own config (I have a `vitest.real.config.js` pattern that
  sets `fileParallelism: false` — also needed because those suites share one SQLite
  file and otherwise fail with `database is locked`), or
- have the setup file skip when `RUN_REAL`/`RUN_E2E` is set.

Also note `tests/e2e/README.md` documents running the real suites with
`--config tests/vitest.config.js`, which contradicts the newer
`tests/e2e/real/README.md`; worth reconciling in the same change.

**2. `tests/unit/zed-native-auth.test.js` — apply the `975f28a5` pattern** (or
`vi.hoisted`, if static imports are kept).

**3. Add a CI test job.** There is currently none, so a regression is invisible until
someone inspects their dashboard.

**4. Optional guard.** A cheap test asserting `process.env.DATA_DIR` is set before the
first `getAdapter()` call would make this permanently self-checking.

## Notes on blast radius

For the record, I checked what *wasn't* affected, in case it is useful:

- `settings` — clean (41 keys, none matching test sentinels such as `field0…field49`
  from `tests/unit/db-concurrent.test.js:98` or `cloudEnabled` from
  `tests/unit/db-sqlite-vs-lowdb.test.js:33`).
- `usageHistory` (65,581 rows), `usageDaily`, `requestDetails`, `apiKeys`,
  `providerNodes`, `proxyPools`, `combos` — all genuine data; no `testprov`/`bench-p*`/
  `oauth-test` fixtures.
- `src/lib/usageDb.js:1-9` is a 9-line re-export shim over `@/lib/db/index.js`, so it
  **does** honour `DATA_DIR` (contrary to a stale note in `CLAUDE.md` claiming
  `usage.json`/`log.txt` live outside `DATA_DIR` — worth correcting separately).
- Hardcoded `~/.9router` paths that ignore `DATA_DIR`, a latent risk if a test ever
  exercises them: `src/lib/mitmAliasCache.js:10-11`, `src/lib/appUpdater.js:16,102,104`,
  `src/lib/updater/updater.js:27,29`, `tests/unit/antigravity-cache.test.js:22`
  (the last is `describe.skipIf` unless `AG_CACHE_TEST=1`).
- Exactly 25 of the ~340 collected test files set `DATA_DIR`; the other 315 reach DB
  code only through `vi.mock`/`vi.doMock` of `@/lib/db/driver.js`, `@/lib/localDb`,
  `@/lib/usageDb.js`, `@/lib/db/index.js`, or `@/models`. No `src/` module performs a DB
  call at import time, so merely importing a DB module cannot pollute — only an actual
  repo call can.

## Environment

- `decolua/9router` @ `master`, commit `975f28a5` (2026-09-26)
- Node 22.x, vitest 4.1.10, default pool (`forks`), `isolate: true`
- Observed on Linux; `dataDir.js` has an equivalent `win32` branch (`APPDATA`), so the
  same applies on Windows

## Related (adjacent, not duplicates)

- #138 — `usageDb`/`requestDetailsDb` ignoring `DATA_DIR` (production direction, closed)
- #2814 — self-update relaunch dropping `DATA_DIR`, falling back to a fresh `~/.9router` (open)
- #399 — "fix: respect DATA_DIR and XDG_CONFIG_HOME in all data paths" (merged, runtime)
- PR #2605 — establishes the per-file temp-`DATA_DIR` pattern in
  `codex-multiaccount-persistence.test.js`, but adds no config-level isolation (open)

None of these cover tests writing to the production DB.
