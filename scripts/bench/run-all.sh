#!/usr/bin/env bash
# Reproduces every run behind docs/benchmarks/REPORT.md, sequentially (one server at a time).
# Needs: a built node variant (`npm run build`), Node >= 22.5 (node:sqlite), taskset (optional).
#
#   scripts/bench/run-all.sh [group...]            groups: main1 main2 main3 delay logerror knee ab (default: all)
#   BENCH_OUT=docs/benchmarks/runs/pre-fix scripts/bench/run-all.sh   (write somewhere else)
#   BENCH_SRC=/path/to/data.sqlite                  (default ~/.9router/db/data.sqlite, copied + scrubbed)
#
# Each group is a separate invocation so it can be run as its own shell command (a group takes
# roughly 5-14 minutes). Servers are started fresh per scenario.
set -euo pipefail
cd "$(dirname "$0")/../.."
SRC="${BENCH_SRC:-$HOME/.9router/db/data.sqlite}"
OUT="${BENCH_OUT:-docs/benchmarks/runs}"
COMMON=(--src "$SRC" --server-cpus 0-3 --mock-cpus 4-5)
GROUPS_TO_RUN=("$@"); [ ${#GROUPS_TO_RUN[@]} -eq 0 ] && GROUPS_TO_RUN=(main1 main2 main3 delay logerror knee ab)

run() { # name args...
  local name="$1"; shift
  rm -rf "$OUT/$name"; mkdir -p "$OUT/$name"
  node scripts/bench/run-bench.mjs "${COMMON[@]}" --out "$OUT/$name" "$@" 2>&1 | tee "$OUT/$name/run.log"
}

for g in "${GROUPS_TO_RUN[@]}"; do
  case "$g" in
    # Main matrix: default (INFO) server logging, instant mock upstream.
    main1) run node-main-1 --scenarios health,models,chat-401 ;;
    main2) run node-main-2 --scenarios chat-ns-plain,chat-stream-plain ;;
    main3) run node-main-3 --scenarios chat-ns-acl,chat-stream-acl --open-scenario chat-ns-plain --open-rates 100,200,250,300,350,400 ;;
    # Mock upstream with 50 ms latency (closer to a real provider): where does concurrency saturate?
    delay) run node-mock-delay-50ms --scenarios chat-ns-plain --concurrency 1,4,8,16,32,64,128,512,1024 --mock-delay 50 ;;
    # Same as main but with per-request INFO logging silenced: cost of the default logging.
    logerror) run node-log-error --scenarios health,chat-ns-plain --concurrency 1,8,32,128 --log-level ERROR ;;
    # Open-loop sweep through the saturation knee (no closed-loop scenarios).
    knee) run node-open-knee --scenarios "" --open-scenario chat-ns-plain --open-rates 450,500,550,600 ;;
    # A/B for a code change: run on each build, keep the output dirs apart via BENCH_OUT.
    ab) run "${AB_NAME:-ab}" --scenarios chat-ns-plain,chat-stream-plain --concurrency 1,8,32 --repeats 4 ;;
    *) echo "unknown group $g" >&2; exit 2 ;;
  esac
done
