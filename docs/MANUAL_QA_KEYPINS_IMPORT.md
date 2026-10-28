# Manual QA: key routing pins and staged backup import

Checklist for two dashboard features. Expected results come from the UI and API code
(`src/app/(dashboard)/dashboard/endpoint/EndpointPageClient.js`, `src/app/api/keys/**`, `src/lib/apiKeys/forcePins.js`,
`forcePair.js`, `src/app/(dashboard)/dashboard/profile/page.js`, `src/lib/importProgress.js`); they have not been clicked
through in a browser. Automated coverage: `tests/unit/api-key-force-*.test.js`, `tests/unit/import-progress.test.js`, `tests/unit/merge-backups.test.js`.

**Set-up (use a throwaway instance, Replace mode wipes data)**

```sh
export DATA_DIR=$(mktemp -d)            # empty database, never your real ~/.9router
PORT=20127 node custom-server.js         # after `npm run build`; log in, add one OpenAI-style provider connection
```

You need the dashboard password for the import steps (do not paste it into tickets or logs). Have two different
provider ids available on the Providers page (examples below use `openai` and `anthropic`).

## A. Forced Routing on the Endpoint page (`/dashboard/endpoint`)

1. **Add modal has the section.** Under the key list click **Create Key**. The modal "Create API Key" shows, between the IP
   rules and **Expires At**, a **Forced Routing** block with the help text "A pinned model overrides whatever model the client
   asks for ...", an input **Forced Provider** (placeholder `e.g. openai`, browser suggestions list the providers of your active
   connections) and **Forced Model** (placeholder `provider/model, e.g. openai/gpt-4o`).
2. **Edit modal has the section, prefilled.** Click the gear icon (tooltip "Edit key settings", visible on hover on wide screens)
   on an existing key. The modal "Edit Key: <name>" shows the same block, filled with the key's current pins (empty when none).
3. **`Pinned:` badge.** Create a key with Forced Model `openai/gpt-4o` and no provider. The key row shows a cyan badge
   `Pinned: openai/gpt-4o`; hover tooltip `Pinned: model openai/gpt-4o`. Edit it: set Forced Provider `openai` too. Badge text
   stays `Pinned: openai/gpt-4o` (model wins) and the tooltip becomes `Pinned: provider openai, model openai/gpt-4o`. A key with
   only Forced Provider `openai` shows `Pinned: openai`. A key with neither shows no badge.
4. **Trimming and bad characters.** Enter ` openai ` (spaces around) as Forced Provider and save: it is stored trimmed (reopen
   Edit, no spaces). Enter `open ai` (inner space): the modal stays open and a red message appears directly under the
   Description field at the top of the modal (scroll up): `forceProvider must not contain whitespace or control characters`.
   Nothing is created or changed.
5. **Mismatched pair is rejected.** Forced Provider `anthropic` + Forced Model `openai/gpt-4o`, then Create/Save. Expected red
   message: `forceModel 'openai/gpt-4o' routes to provider 'openai', which does not match forceProvider 'anthropic'`. The modal
   stays open, the key list is unchanged. Change the provider to `openai`: it saves.
6. **Unresolvable model.** Forced Provider `openai` + Forced Model `mystery-model-xyz` (no slash, unknown): red message
   `forceModel 'mystery-model-xyz' does not resolve to a provider, so it cannot satisfy forceProvider 'openai'`. A combo whose
   members span two providers is rejected with the `does not match` message the same way.
7. **Provider-prefix normalisation.** A bare model that belongs to the pinned provider is accepted: Forced Provider `openai` +
   Forced Model `gpt-4o` saves. A model written with a provider alias prefix resolves to the canonical provider id before the
   comparison: pick an alias from your Providers page (for example the short prefix a provider shows next to its models), set
   Forced Model `<alias>/<model>` and Forced Provider = the **canonical id** (accepted) and then = the alias itself (rejected,
   the message names the canonical provider). Skip this step if your instance has no aliases.
8. **Blocked-list conflicts.** In the same modal put `openai/gpt-4o` in **Blocked Models** and Forced Model `gpt-4o` (bare):
   rejected with `forceModel must not be in blockedModels` (a bare pin is covered by a prefixed blocked entry). Put `openai` in
   **Blocked Providers** (API only: `PUT /api/keys/<id>` with `blockedProviders`) with Forced Provider `openai`: rejected with
   `forceProvider must not be in blockedProviders`.
9. **Clearing.** Edit a pinned key, empty both fields, **Save Changes**: the badge disappears, and re-opening Edit shows empty fields.
10. **Runtime effect** (the pins are only worth anything if enforcement honours them):

    ```sh
    curl -s localhost:20127/v1/chat/completions -H "Authorization: Bearer <pinned key>" -H 'content-type: application/json' \
      -d '{"model":"whatever/ignored","messages":[{"role":"user","content":"hi"}]}'
    ```

    Server log shows `[apiKey] force routing: asked="whatever/ignored" -> "openai/gpt-4o"` and the upstream call goes to the pinned
    model. For a key with only a Forced Provider, asking for a model of another provider returns HTTP 403
    `This API key is pinned to provider 'openai'`. `GET /v1/models` with the pinned key lists only the pinned scope.

## B. Staged progress for Import Backup and Merge Backup (`/dashboard/profile`)

Prepare two files first with **Download Backup** (card with "Database Location"): a small one (fresh instance) and, if you can,
a large one (several MB: import many custom models or connections). Keep a copy of each.

1. **Import Backup, Merge mode.** Click **Import Backup**, pick the file. The "Confirm Password" modal has **Import Mode**
   (default `Merge - keep existing data (recommended)`; the other option is Replace) and **Conflict Strategy** (Newer Wins,
   Incoming Wins, Keep Current, Older Wins). Enter the password, **Confirm**. A status line with a spinning icon (screen-reader
   `role=status`) walks through, in this order:
   `Reading backup file (<size>)` -> `Validating backup` -> `Loading current database` -> `Merging` -> `Preparing upload` ->
   `Uploading and restoring on the server (this can take a while for large backups)` -> `Refreshing settings`.
   After a stage has lasted one second it ends with ` - 3s elapsed`, ticking every second. With the small file most stages flash by;
   with the large file `Merging` and `Uploading and restoring` stay long enough to read, and the elapsed counter keeps counting.
   While it runs **Import Backup** and **Merge Backup** are disabled.
2. **Merge result message** (green, replaces the spinner line):
   `Import (merge) complete: A added, U updated, N unchanged [wrote <counts>; server <time>] (took <time>)`.
   `<counts>` lists only non-empty tables, for example `42 connections, 5 API keys, 8 combos`; `server` is the server-side time
   (`340ms` or `1m 05s` style). The counts are rows actually written (distinct rows): compare `connections` / `API keys` with the
   Providers and Endpoint pages afterwards. Importing the **same file again** gives `0 added, 0 updated, N unchanged`.
3. **Conflict strategy.** In a copy of the backup JSON change one connection's `name` and set its `updatedAt` to a later date.
   Merge it with **Newer Wins**: `1 updated`, the new name appears. Repeat with a copy whose `updatedAt` is older: `0 updated`.
   **Keep Current** never overwrites; **Incoming Wins** always does.
4. **Import Backup, Replace mode.** Choose **Replace - wipe current data and restore backup** (confirm you are on the throwaway
   instance). Stages are shorter, with no loading/merge step: `Reading backup file (<size>)` -> `Validating backup` ->
   `Preparing upload` -> `Uploading and restoring on the server ...` -> `Refreshing settings`. Success text:
   `Database imported (replaced) successfully [wrote <counts>; server <time>] (took <time>)`. The data now equals the backup.
5. **Merge Backup button (separate path).** Click **Merge Backup**, pick a file. The file is parsed and validated *before* the
   password modal: invalid JSON gives the red `Failed to parse backup file`; JSON that is not a backup gives
   `Invalid backup: <reasons>`; in both cases no modal opens and nothing is touched. A valid file opens the modal with only the
   **Conflict Strategy** select. After **Confirm** the stages are `Loading current database` -> `Merging` -> `Preparing upload` ->
   `Uploading and restoring ...` -> `Refreshing settings`, ending with
   `Merge complete: A added, U updated, N unchanged [wrote <counts>; server <time>] (took <time>)`.
6. **Failures clear the progress.** Use a wrong password: the spinner line disappears, a red message shows the server's text
   (`Invalid password or admin token`), buttons are enabled again, data unchanged. Import a non-JSON file in Merge mode: red
   `Unexpected token ...`-style parse error after `Validating backup`; a JSON file without backup structure: `Invalid backup: ...`.
   **Cancel** in the password modal does nothing and shows no progress.
7. **No stale state.** After any run (success or failure) the spinner line is gone, the buttons work, and starting a second
   import shows `Reading backup file` again with the elapsed time starting from zero.
8. **Large backup responsiveness.** During the long stages the page stays interactive (you can scroll and open other tabs);
   only the two import buttons are disabled. If the browser tab freezes for more than a few seconds during `Merging`, record the
   backup size and table counts: that is the O(n^2) merge regression described in `src/lib/db/mergeBackups.js`.

Record for each step: pass/fail, browser, backup sizes, and the exact text of any message that differs from the above.
