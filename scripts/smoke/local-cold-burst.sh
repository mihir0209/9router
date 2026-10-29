#!/usr/bin/env bash
# Cold-isolate concurrent burst on workerd (`wrangler dev --local`).
#
# Why: a promise started by one Workers request and awaited by another can hang forever or throw
# "Cannot perform I/O on behalf of a different request". Node cannot reproduce that, workerd can.
# Every round restarts wrangler (a fresh isolate), then fires ~20 concurrent requests (12 x GET
# /v1/models, 4 x health, 2 x /login, 2 x rejected chat) and checks that all of them finish quickly.
# It also checks If-None-Match handling on /v1/models (strong, weak, list, `*`, mismatch).
#
# Needs a built Cloudflare bundle (`npm run build:cloudflare`). Uses a throwaway local D1 (never the
# deployed one) and random local-only secrets.
#
#   scripts/smoke/local-cold-burst.sh [rounds]            # default 3
#   SEED_SQL=backup.sql scripts/smoke/local-cold-burst.sh # seed the local D1 with `wrangler d1 export` output
#   PORT=8799 FAIL_AFTER_S=20 scripts/smoke/local-cold-burst.sh
set -u
cd "$(dirname "$0")/../.."
ROUNDS=${1:-3}
PORT=${PORT:-8799}
FAIL_AFTER_S=${FAIL_AFTER_S:-20}
STATE=$(mktemp -d)
ENVF=$(mktemp)
LOGDIR=$(mktemp -d)
B=http://127.0.0.1:$PORT
trap 'pkill -f "[w]rangler-dist" 2>/dev/null; pkill -f "[w]orkerd" 2>/dev/null; rm -rf "$STATE" "$ENVF"' EXIT

ADMIN=$(openssl rand -hex 16)
chmod 600 "$ENVF"
cat > "$ENVF" <<EOF
ADMIN_API_TOKEN=$ADMIN
JWT_SECRET=$(openssl rand -hex 24)
INITIAL_PASSWORD=$(openssl rand -hex 8)
API_KEY_SECRET=$(openssl rand -hex 16)
MACHINE_ID_SALT=$(openssl rand -hex 8)
EOF

if [ -n "${SEED_SQL:-}" ]; then
  npx wrangler d1 execute DB --local --persist-to "$STATE" --file "$SEED_SQL" > /dev/null 2>&1 || { echo "seed failed"; exit 2; }
fi

boot() {
  npx wrangler dev --local --port "$PORT" --persist-to "$STATE" --env-file "$ENVF" --log-level warn > "$LOGDIR/dev-$1.log" 2>&1 &
  for _ in $(seq 1 60); do (echo > /dev/tcp/127.0.0.1/$PORT) 2>/dev/null && break; sleep 0.5; done
  sleep 3
}
stop() { pkill -f "[w]rangler-dist" 2>/dev/null; pkill -f "[w]orkerd" 2>/dev/null; sleep 2; }

# 1. create a throwaway key in a first boot
boot 0
KEY=""
for _ in 1 2 3 4 5 6; do
  KEY=$(curl -s -m 60 -X POST "$B/api/admin/keys" -H "authorization: Bearer $ADMIN" -H 'content-type: application/json' \
    -d '{"ownerRef":"local-burst","name":"burst-local"}' | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{try{console.log(JSON.parse(s).key||"")}catch{console.log("")}})')
  [ -n "$KEY" ] && break; sleep 2
done
[ -n "$KEY" ] || { echo "FAIL: could not create a local key"; exit 2; }
sleep 3; stop

fail=0
for r in $(seq 1 "$ROUNDS"); do
  boot "$r"   # fresh isolate: nothing cached yet
  out=$(mktemp); PIDS=()
  t0=$(date +%s.%N)
  for _ in $(seq 1 12); do curl -s -o /dev/null -w "models %{http_code} %{time_total}\n" --max-time 60 -H "authorization: Bearer $KEY" "$B/v1/models" >> "$out" & PIDS+=($!); done
  for _ in $(seq 1 4);  do curl -s -o /dev/null -w "health %{http_code} %{time_total}\n" --max-time 60 "$B/api/health" >> "$out" & PIDS+=($!); done
  for _ in $(seq 1 2);  do curl -s -o /dev/null -w "login %{http_code} %{time_total}\n" --max-time 60 "$B/login" >> "$out" & PIDS+=($!); done
  for _ in $(seq 1 2);  do curl -s -o /dev/null -w "chat-bad %{http_code} %{time_total}\n" --max-time 60 -X POST -H "authorization: Bearer $KEY" -H 'content-type: application/json' -d '{"model":"nope/none","messages":[{"role":"user","content":"hi"}]}' "$B/v1/chat/completions" >> "$out" & PIDS+=($!); done
  wait "${PIDS[@]}"
  t1=$(date +%s.%N)
  wall=$(echo "$t1 - $t0" | bc)
  n=$(wc -l < "$out")
  echo "== round $r: $n responses, wall ${wall}s"
  sort "$out" | awk '{k=$1" "$2; c[k]++; if($3>m[$1]) m[$1]=$3} END{for(k in c) print "  ", k, "x" c[k]; for(k in m) print "   max", k, m[k] "s"}' | sort
  bad=$(awk '$2>=500 || $2==0' "$out" | wc -l)
  slow=$(awk -v lim="$FAIL_AFTER_S" '$3>lim' "$out" | wc -l)
  [ "$n" -eq 20 ] && [ "$bad" -eq 0 ] && [ "$slow" -eq 0 ] || { echo "  FAIL: expected 20 responses without 5xx/timeouts (got $n, $bad bad, $slow slow)"; fail=1; }

  H=$(curl -s -D - -o /dev/null -H "authorization: Bearer $KEY" "$B/v1/models" | tr -d '\r' | awk -F': ' 'tolower($1)=="etag"{print $2}')
  S=${H#W/}
  expect() { got=$(curl -s -o /dev/null -w '%{http_code}' -H "authorization: Bearer $KEY" -H "if-none-match: $1" "$B/v1/models"); [ "$got" = "$2" ] && echo "   INM $3 -> $got ok" || { echo "   INM $3 -> $got (expected $2) FAIL"; fail=1; }; }
  if [ -n "$H" ]; then
    expect "$S" 304 "strong"; expect "W/$S" 304 "weak"; expect "\"zzz\", W/$S" 304 "list+weak"; expect "*" 304 "star"; expect "\"zzz\"" 200 "mismatch"
  else echo "   no ETag returned FAIL"; fail=1; fi
  rm -f "$out"; stop
done
grep -il "different request\|never resolve" "$LOGDIR"/dev-*.log > /dev/null 2>&1 && { echo "FAIL: cross-request I/O messages in the dev logs"; fail=1; }
[ "$fail" -eq 0 ] && echo "PASS: no hangs, no 5xx, conditional requests behave" || echo "FAILED"
exit "$fail"
