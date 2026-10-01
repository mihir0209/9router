# Azure: stored `deployment` overrides the requested model

## Status
Confirmed by code inspection, local database inspection, and upstream research.
Not yet fixed. No PR opened — awaiting a decision.

## The mechanism

`open-sse/executors/azure.js:17-20`:

```js
const deployment = credentials?.providerSpecificData?.deployment
  || model
  || process.env.AZURE_DEPLOYMENT
  || "gpt-4";

return `${endpoint}/openai/deployments/${deployment}/chat/completions?api-version=${apiVersion}`;
```

The stored `deployment` is **first** in the `||` chain, so it always wins.
`model` (what the client actually asked for) is only reached when `deployment`
is falsy. Azure's `/deployments/{d}/chat/completions` route is keyed off the URL
path, so the `model` field still present in the request body is ignored upstream.
The two can therefore disagree silently.

Two further details:

- `process.env.AZURE_DEPLOYMENT` is effectively dead code — with the ordering
  `deployment || model || AZURE_DEPLOYMENT` the env var can only win if both the
  stored deployment *and* the model are empty.
- Nothing validates the mismatch or warns the admin.

## The configured model pool does not affect dispatch

The "configurable pool" on the provider-specific page is read in exactly one
place, `src/lib/modelCatalog/providerModelResolver.js:260-267`, and its only
consumers are `/v1/models` and `/api/models/refresh`. It shapes what is
**advertised**, never what is **dispatched**. No code writes `models` or
`enabledModels` into an Azure connection's `providerSpecificData` either.

So for Azure the two are fully decoupled: the pool decides what a client sees,
and `deployment` decides what is actually called.

## What this instance actually looks like

All four Azure connections are pinned, and none has a pool configured:

| Connection | `deployment` (sent upstream) | advertised pool |
|---|---|---|
| `grok 4.6` | `grok-4.6` | none |
| `Prod- south india` | `gpt-5.6-terra` | none |
| `East Us 2` | `gpt-5.6-sol` | none |
| `YC kimi k3` | `FW-Kimi-K3` | none |

Two consequences:

1. **The pin is currently load-bearing.** Every Azure request lands on its
   connection's hardcoded deployment. The empty pool is the only reason the
   UI/config story has never been exercised.
2. **Cross-connection wrong-model delivery is possible.** Four *enabled*
   connections span four *different* deployments. Account selection chooses
   among enabled connections, so a request for `azure/gpt-5.6-terra` can be
   served by `East Us 2` (`gpt-5.6-sol`) or `YC kimi k3` (`FW-Kimi-K3`) and
   silently return a model the client never asked for. This is the sharpest
   form of the bug and does not depend on the pool UI at all.

## Upstream research (`decolua/9router`, branch `master`)

- Upstream `open-sse/executors/azure.js` has **identical** precedence. `git log -L`
  shows exactly one commit ever touched those lines: `65f11a60`, the original
  add. It has never been changed.
- Six Azure issues/PRs exist (709, 706, 972, 2691, 2809, 4475). **None**
  describes the deployment overriding the requested model.
- Eleven further targeted queries found nothing. Every upstream artifact treats
  a per-connection deployment as an intentional single-deployment pin.
- Upstream ships no Azure model catalog at all, so Azure's advertised models come
  only from the user-entered pool — plausibly why the maintainer treats that field
  as documentation and the pin as intentional.

Local `transformRequest` differs from upstream (we call `super.transformRequest`,
upstream returns `body`); that difference already overlaps unmerged upstream
PR #2691 / #2809.

## Fix shape (backward compatible)

Do **not** simply swap the precedence. With no pool configured, that would send
`deployment` *values* as URL path segments and produce 404s on every connection.

Instead: keep the stored deployment as the default, and let the requested model
win **only when it is explicitly listed in that connection's configured pool**.

- No pool configured (all four connections today) → behaviour unchanged.
- Pool contains the requested model → that model is used.
- Single-deployment users → unchanged.

This also gives the pool UI on the provider page an actual effect for Azure
instead of being display-only.

## Open questions for the decision

- Should the pool be **per-connection** (current shape) or **per-provider**?
- Should a requested model that is in no Azure connection's pool be rejected
  rather than silently served by a pinned deployment?
- Should the admin UI warn when a connection advertises models its deployment
  cannot serve?