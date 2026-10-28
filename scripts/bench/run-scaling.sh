#!/bin/sh
# Runs scripts/bench/scaling.mjs with every proxy variable removed from the ORCHESTRATOR (sandboxes that force
# HTTP(S)_PROXY / NODE_USE_ENV_PROXY=1 would otherwise send the loopback health checks through the proxy).
# The servers it starts get a dead proxy of their own, set inside scaling.mjs.
#   scripts/bench/run-scaling.sh --servers 4 --src ~/.9router/db/data.sqlite --out docs/benchmarks/runs/scaling/n4
cd "$(dirname "$0")/../.." || exit 1
exec env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy -u ALL_PROXY -u all_proxy -u NODE_USE_ENV_PROXY \
  NO_PROXY=127.0.0.1,localhost node scripts/bench/scaling.mjs "$@"
