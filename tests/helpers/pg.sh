#!/usr/bin/env bash
# Start a throwaway PostgreSQL for the integration tests.
#
#   tests/helpers/pg.sh up      # start, and print the URL to export
#   tests/helpers/pg.sh down    # stop and remove
#
# The container is deliberately separate from the app's own database: these
# tests DROP and recreate the public schema, so pointing them at a real
# DATABASE_URL would destroy it.
set -euo pipefail

NAME="${PG_CONTAINER:-n9router-test-pg}"
PORT="${PG_PORT:-5433}"
DB="${PG_DB:-n9router_test}"
PASS="${PG_PASSWORD:-testpw}"
IMAGE="${PG_IMAGE:-postgres:17-alpine}"

case "${1:-up}" in
  up)
    if docker ps -a --format '{{.Names}}' | grep -qx "$NAME"; then
      docker rm -f "$NAME" >/dev/null
    fi
    docker run -d --name "$NAME" \
      -e POSTGRES_PASSWORD="$PASS" -e POSTGRES_DB="$DB" \
      -p "$PORT:5432" "$IMAGE" >/dev/null
    for _ in $(seq 1 40); do
      if docker exec "$NAME" pg_isready -U postgres >/dev/null 2>&1; then break; fi
      sleep 0.5
    done
    echo "export TEST_DATABASE_URL=postgres://postgres:$PASS@127.0.0.1:$PORT/$DB"
    ;;
  down)
    docker rm -f "$NAME" >/dev/null 2>&1 || true
    echo "removed $NAME"
    ;;
  *)
    echo "usage: $0 [up|down]" >&2
    exit 2
    ;;
esac
