#!/usr/bin/env bash
# Deploy 9router to Antideploy over their REST API.
#
#   scripts/antideploy.sh            # deploy
#   scripts/antideploy.sh --refresh-all      # also re-read models from providers
#
# Credentials live in ~/.antideploy/config.json (mode 0600) or $ANTIDEPLOY_KEY.
# The account token flow needs one browser approval, once:
#
#   curl -sS -X POST https://antideploy.com/api/v1/device/code \
#     -H 'content-type: application/json' -d '{"clientName":"opencode"}' -o /tmp/ad.json
#   # open $(jq -r .verificationUriComplete /tmp/ad.json) and approve, then:
#   DEVICE=$(jq -r .deviceCode /tmp/ad.json)
#   curl -sS -X POST https://antideploy.com/api/v1/device/token \
#     -H 'content-type: application/json' -d "{\"deviceCode\":\"$DEVICE\"}" \
#     | jq -r .token > ~/.antideploy/config.json && chmod 0600 ~/.antideploy/config.json
#
# Limits this script works within: 28 MB and 4,000 files per push, 20 deploys an
# hour, 10 successful deploys a month on the free plan.
set -euo pipefail

APP_ID="${ANTIDEPLOY_APPLICATION_ID:-}"
KEY="${ANTIDEPLOY_KEY:-}"
CONFIG="${ANTIDEPLOY_CONFIG:-$HOME/.antideploy/config.json}"

[[ -n "$KEY" && -z "$APP_ID" ]] || true
if [[ -z "$KEY" && -f "$CONFIG" ]]; then
  # Either shape: a bare token, or {"token": "..."}.
  KEY="$(node -e 'const fs=require("fs");const raw=fs.readFileSync(process.argv[1],"utf8");let v=raw.trim();if(v.startsWith("{")){v=JSON.parse(v).token;}if(!v)process.exit(1);process.stdout.write(v);' "$CONFIG")"
fi
if [[ -z "$KEY" ]]; then
  echo "no credential: set ANTIDEPLOY_KEY or create $CONFIG (see the header of this script)" >&2
  exit 1
fi
if [[ -z "$APP_ID" ]]; then
  if [[ -f .antideploy.json ]]; then
    APP_ID="$(node -e 'process.stdout.write(JSON.parse(require("fs").readFileSync(".antideploy.json","utf8")).applicationId)')"
  else
    echo "no applicationId: set ANTIDEPLOY_APPLICATION_ID, or run the create step first" >&2
    exit 1
  fi
fi

# tests/ is ~1,050 files of test-only material. It is not needed to serve the
# app, and excluding it keeps the push well inside both limits.
EXCLUDES=(--exclude=.git --exclude=node_modules --exclude=.next --exclude=.open-next
          --exclude=tests --exclude=./tests/node_modules --exclude='*.log')

echo "packing (excluding tests/, .next/, node_modules/)…"
FILES=$(find . -type f \
  -not -path './.git/*' -not -path './node_modules/*' -not -path './.next/*' \
  -not -path './.open-next/*' -not -path './tests/*' | wc -l)
echo "  $FILES files"

# Secrets are write-only over the API; the env travels with the push.
ENV_ARGS=()
if [[ -f .env.deploy ]]; then
  ENV_ARGS=(-F "env=$(cat .env.deploy)")
elif [[ -f .env ]]; then
  ENV_ARGS=(-F "env=$(cat .env)")
fi

RESPONSE=$(tar czf - "${EXCLUDES[@]}" . | \
  curl -sS -X POST "https://antideploy.com/api/v1/deploy?applicationId=$APP_ID" \
    -H "Authorization: Bearer $KEY" \
    -F "archive=@-")
echo "$RESPONSE" | head -c 400; echo

STATUS=$(echo "$RESPONSE" | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{try{process.stdout.write(String(JSON.parse(s).status||"?"))}catch{process.stdout.write("?")}})')
WATCH=$(echo "$RESPONSE" | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{try{process.stdout.write(String(JSON.parse(s).watch||""))}catch{}})')

if [[ "$STATUS" == "unchanged" ]]; then
  echo "content hash unchanged — nothing built (use force=true to rebuild)"
  exit 0
fi
if [[ -z "$WATCH" ]]; then
  echo "no watch URL in the response" >&2
  exit 1
fi

echo "watching…"
for _ in $(seq 1 180); do
  sleep 5
  POLL=$(curl -sS "$WATCH" -H "Authorization: Bearer $KEY")
  STATE=$(echo "$POLL" | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{try{process.stdout.write(String(JSON.parse(s).status||"?"))}catch{process.stdout.write("?")}})')
  printf '\r  %s' "$STATE"
  case "$STATE" in
    succeeded) echo; break ;;
    failed)    echo; echo "$POLL" | head -c 3000; echo; exit 1 ;;
  esac
done

# warnings and hazards are the documented way a deploy can succeed and still not
# do what its author meant. Both are reported, not ignored.
echo "$POLL" | node -e '
let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{
  const d=JSON.parse(s);
  for (const w of d.warnings||[]) console.log("  WARNING:", typeof w==="string"?w:JSON.stringify(w));
  for (const h of d.hazards||[]) console.log("  HAZARD :", typeof h==="string"?h:JSON.stringify(h));
  if (d.deployment?.url) console.log("  live at:", d.deployment.url);
  if (d.security?.issues?.length) console.log("  SECURITY:", JSON.stringify(d.security.issues));
});'
