#!/usr/bin/env bash
set -euo pipefail

if [[ "${NOOK_DOCKER_E2E:-}" != "1" ]]; then
  echo "set NOOK_DOCKER_E2E=1 to run Docker integration tests"
  exit 0
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
compose_file="${NOOK_DOCKER_COMPOSE:-$repo_root/docker/compose.yaml}"
project_name="nook-e2e-${RANDOM}-$$"
test_root="$(mktemp -d)"
run_pid=""

cleanup() {
  result=$?
  if [[ "$result" -ne 0 ]]; then
    echo "Docker E2E failed; Nook stderr:" >&2
    test ! -f "$test_root/run.err" || cat "$test_root/run.err" >&2
    echo "Caddy logs:" >&2
    docker compose -p "$project_name" -f "$compose_file" logs caddy >&2 || true
  fi
  if [[ -n "$run_pid" ]]; then
    kill -TERM "$run_pid" 2>/dev/null || true
    wait "$run_pid" 2>/dev/null || true
  fi
  docker compose -p "$project_name" -f "$compose_file" down --volumes --remove-orphans >/dev/null 2>&1 || true
  rm -rf "$test_root"
  return "$result"
}
trap cleanup EXIT

cd "$repo_root"
cargo build --locked
docker compose -p "$project_name" -f "$compose_file" config --quiet
docker compose -p "$project_name" -f "$compose_file" up --detach --wait

mkdir -p "$test_root/config/nook" "$test_root/state"
cp docker/nook-config.toml.example "$test_root/config/nook/config.toml"
export XDG_CONFIG_HOME="$test_root/config"
export XDG_STATE_HOME="$test_root/state"
export NOOK_DISABLE_UPDATE_CHECK=1

target/debug/nook status >"$test_root/status.txt" 2>"$test_root/status.err"
grep -q $'caddy\tok' "$test_root/status.txt"
grep -q $'caddy_loopback_host\thost.docker.internal' "$test_root/status.txt"

target/debug/nook ca export "$test_root/caddy-local-ca.pem" >"$test_root/ca.txt"
grep -Eq '^sha256=[0-9a-f]{64}$' "$test_root/ca.txt"
first_fingerprint="$(grep '^sha256=' "$test_root/ca.txt")"

target/debug/nook run --name docker-run --app-port 38080 --strict-port -- \
  python3 -c 'import http.server,os; http.server.ThreadingHTTPServer((os.environ["HOST"],int(os.environ["PORT"])),http.server.SimpleHTTPRequestHandler).serve_forever()' \
  >"$test_root/run.out" 2>"$test_root/run.err" &
run_pid=$!

for _ in {1..100}; do
  if curl --silent --fail --cacert "$test_root/caddy-local-ca.pem" \
    --resolve docker-run.localhost:443:127.0.0.1 \
    https://docker-run.localhost/ >/dev/null; then
    break
  fi
  sleep 0.1
done

curl --silent --fail --cacert "$test_root/caddy-local-ca.pem" \
  --resolve docker-run.localhost:443:127.0.0.1 https://docker-run.localhost/ >/dev/null

target/debug/nook alias set docker-http 38080 --no-tls >/dev/null
curl --silent --fail --resolve docker-http.localhost:80:127.0.0.1 \
  http://docker-http.localhost/ >/dev/null
container_body="$(docker compose -p "$project_name" -f "$compose_file" exec -T caddy \
  wget --quiet --header='Host: docker-http.localhost' --output-document=- http://127.0.0.1/)"
[[ "$container_body" != *"Directory listing for /"* ]]

docker compose -p "$project_name" -f "$compose_file" restart caddy
docker compose -p "$project_name" -f "$compose_file" up --detach --wait
target/debug/nook ca export "$test_root/caddy-local-ca-after.pem" >"$test_root/ca-after.txt"
[[ "$first_fingerprint" == "$(grep '^sha256=' "$test_root/ca-after.txt")" ]]

target/debug/nook status >/dev/null 2>"$test_root/status-after.err"
curl --silent --fail --cacert "$test_root/caddy-local-ca.pem" \
  --resolve docker-run.localhost:443:127.0.0.1 https://docker-run.localhost/ >/dev/null

if [[ "$compose_file" == *caddy-docker-proxy* ]]; then
  embedded="$(docker compose -p "$project_name" -f "$compose_file" exec -T caddy caddy version)"
  [[ "$embedded" == v2.11.* ]]
  docker run --detach --rm --name "$project_name-trigger" \
    --network nook-caddy-proxy-network --entrypoint sleep \
    --label caddy=event.localhost --label caddy.respond='"event"' \
    caddy:2.11.4-alpine 30 >/dev/null
  sleep 2
  target/debug/nook status >/dev/null 2>"$test_root/proxy-reconcile.err"
  curl --silent --fail --cacert "$test_root/caddy-local-ca.pem" \
    --resolve docker-run.localhost:443:127.0.0.1 https://docker-run.localhost/ >/dev/null
  docker stop "$project_name-trigger" >/dev/null 2>&1 || true
fi

echo "Docker E2E passed with $compose_file"
