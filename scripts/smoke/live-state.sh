#!/usr/bin/env bash
# Read-only check of the live Worker state. Changes nothing, prints no secrets.
#   scripts/smoke/live-state.sh [BASE_URL] [EXPECTED_LIVE_VERSION_PREFIX]
# Another Worker (for example the paid one):
#   LIVE_STATE_CONFIG=wrangler.router.toml scripts/smoke/live-state.sh https://router.<subdomain>.workers.dev <version prefix>
# LIVE_STATE_CONFIG selects the wrangler config (Worker name, account bindings) for the wrangler calls.
# Exit code 0 = live is healthy and on the expected version, 1 otherwise.
set -u
BASE="${1:-https://mihir-router.themihir.workers.dev}"
EXPECTED="${2:-f323aa6a}"
fail=0
CFG=()
[ -n "${LIVE_STATE_CONFIG:-}" ] && CFG=(--config "$LIVE_STATE_CONFIG")

echo "== account"
npx wrangler whoami 2>&1 | grep -Ei "account|logged in" | head -3

echo "== deployments (latest first)"
dep="$(npx wrangler deployments list "${CFG[@]}" 2>&1 | tail -25)"
echo "$dep"
latest="$(echo "$dep" | awk '/^Created:/{c++} c>=1' | tail -n +1)"
if ! echo "$dep" | tail -12 | grep -q "${EXPECTED}.*(100%)"; then
  echo "!! latest deployment does not show ${EXPECTED} at 100%"; fail=1
fi

echo "== secrets (names only)"
npx wrangler secret list "${CFG[@]}" 2>&1 | grep -o '"name": *"[^"]*"' || true

echo "== health / login / static / auth"
code() { curl -s -o /dev/null -m 30 -w "%{http_code}" "$@"; }
h="$(code "$BASE/api/health")";            echo "GET /api/health      -> $h"; [ "$h" = 200 ] || fail=1
l="$(code "$BASE/login")";                 echo "GET /login           -> $l"; [ "$l" = 200 ] || fail=1
m="$(code "$BASE/v1/models")";             echo "GET /v1/models (nokey)-> $m (expect 401)"; [ "$m" = 401 ] || fail=1
c="$(code -X POST "$BASE/api/cron/flush")"; echo "POST /api/cron/flush -> $c (200 = no CRON_SECRET on this version, 401/403 = protected)"

echo "== 60 s of worker outcomes (errors/exceptions only)"
timeout 60 npx wrangler tail "${CFG[@]}" --format json 2>/dev/null \
  | grep -Ei '"outcome":"(exceeded|exception|canceled)' | cut -c1-200 | head -10 || true

[ "$fail" = 0 ] && echo "LIVE OK" || echo "LIVE CHECK FAILED"
exit "$fail"
