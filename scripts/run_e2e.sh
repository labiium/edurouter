#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT=$(pwd)
ENV_FILE=${ENV_FILE:-"${PROJECT_ROOT}/.env"}
RUN_LLM_CHARACTERIZATION=${RUN_LLM_CHARACTERIZATION:-1}
LLM_SAMPLES=${LLM_SAMPLES:-1}
LLM_OUTPUT_PATH=${LLM_OUTPUT_PATH:-"${PROJECT_ROOT}/e2e/llm_characterization.json"}

load_dotenv() {
  local env_path="$1"
  if [[ -f "$env_path" ]]; then
    echo "Loading environment from $env_path"
    set -a
    # shellcheck disable=SC1090
    source "$env_path"
    set +a
  fi
}

normalize_base_url() {
  local raw=${1%/}
  if [[ "$raw" == */v1 ]]; then
    echo "$raw"
  else
    echo "$raw/v1"
  fi
}

resolve_routiium_source() {
  local candidate="${ROUTIIUM_SOURCE_DIR:-${PROJECT_ROOT}/../routiium}"
  if [[ ! -d "$candidate" ]]; then
    cat <<EOF >&2
Error: expected Routiium source directory at '$candidate'.
Clone https://github.com/labiium/routiium as a sibling repo or set ROUTIIUM_SOURCE_DIR=/path/to/routiium before running this script.
EOF
    exit 1
  fi

  if [[ ! -f "$candidate/Cargo.toml" ]]; then
    echo "Error: '$candidate' does not look like a Routiium checkout (Cargo.toml missing)" >&2
    exit 1
  fi

  local resolved
  resolved=$(cd "$candidate" && pwd)
  export ROUTIIUM_SOURCE_DIR="$resolved"
  export ROUTIIUM_DOCKERFILE=${ROUTIIUM_DOCKERFILE:-Dockerfile}

  echo "Using Routiium source: $ROUTIIUM_SOURCE_DIR (Dockerfile: $ROUTIIUM_DOCKERFILE)"
}

wait_for_http() {
  local url=$1
  local timeout=${2:-60}
  local deadline=$((SECONDS + timeout))
  while (( SECONDS < deadline )); do
    if curl -sf "$url" >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  echo "Timed out waiting for $url" >&2
  return 1
}

ensure_routiium_token() {
  if [[ -n ${ROUTIIUM_API_KEY:-} && ${ROUTIIUM_REUSE_API_KEY:-0} -ne 0 ]]; then
    echo "Reusing existing ROUTIIUM_API_KEY from environment"
    return 0
  fi
  if ! command -v python3 >/dev/null 2>&1; then
    echo "python3 is required to run python_tests/generate_api_key.py" >&2
    return 1
  fi
  local base=${ROUTIIUM_HOST_URL%/}
  echo "Requesting Routiium test token via generate_api_key.py..."
  local output
  if ! output=$(python3 python_tests/generate_api_key.py \
    --base-url "$base" \
    --label "e2e-run" \
    --ttl-seconds 900 \
    --json); then
    echo "Failed to invoke generate_api_key.py" >&2
    return 1
  fi
  local token=""
  token=$(
    python3 -c 'import json, sys
try:
    data = json.load(sys.stdin)
except Exception:
    sys.exit(1)
token = data.get("token") or ""
if token:
    print(token)' <<<"$output"
  ) || true
  if [[ -z "$token" ]]; then
    echo "Key generator returned no token; response was:" >&2
    echo "$output" >&2
    return 1
  fi
  export ROUTIIUM_API_KEY="$token"
  echo "Acquired Routiium token"
}

run_llm_characterization() {
  if [[ ${RUN_LLM_CHARACTERIZATION} -ne 1 ]]; then
    echo "Skipping LLM characterization (disabled)"
    return 0
  fi
  if [[ -z ${ROUTIIUM_API_KEY:-} ]]; then
    echo "Skipping LLM characterization; ROUTIIUM_API_KEY unavailable" >&2
    return 1
  fi
  if ! command -v python3 >/dev/null 2>&1; then
    echo "python3 is required to run python_tests/test_openai_models.py" >&2
    return 1
  fi
  local base_env=${ROUTIIUM_BASE_URL:-$(normalize_base_url "$ROUTIIUM_HOST_URL")}
  echo "Running python_tests/test_openai_models.py (samples=${LLM_SAMPLES})"
  mkdir -p "$(dirname "$LLM_OUTPUT_PATH")"
  echo "Using Routiium key ${ROUTIIUM_API_KEY:0:12}... (ttl refresh on each run)"
  ROUTIIUM_BASE_URL="$base_env" \
    ROUTIIUM_API_KEY="$ROUTIIUM_API_KEY" \
    python3 python_tests/test_openai_models.py \
      --samples "$LLM_SAMPLES" \
      --output "$LLM_OUTPUT_PATH"
  echo "Saved LLM characterization report to $LLM_OUTPUT_PATH"
}

load_dotenv "$ENV_FILE"
resolve_routiium_source

if [[ -z ${ROUTIIUM_PORT:-} && -n ${ROUTIIUM_HOST_PORT:-} ]]; then
  ROUTIIUM_PORT="$ROUTIIUM_HOST_PORT"
fi
ROUTIIUM_PORT=${ROUTIIUM_PORT:-8088}
ROUTIIUM_HOST_URL=${ROUTIIUM_HOST_URL:-"http://localhost:${ROUTIIUM_PORT}"}
ROUTIIUM_BIND_ADDR=${ROUTIIUM_BIND_ADDR:-"0.0.0.0:${ROUTIIUM_PORT}"}
ROUTIIUM_ROUTER_URL=${ROUTIIUM_ROUTER_URL:-"http://edurouter:9099"}
ROUTIIUM_SLED_PATH=${ROUTIIUM_SLED_PATH:-"/data/keys.db"}
ROUTIIUM_BASE=${ROUTIIUM_BASE:-$ROUTIIUM_HOST_URL}
ROUTIIUM_BASE_URL=${ROUTIIUM_BASE_URL:-$(normalize_base_url "$ROUTIIUM_HOST_URL")}
export ROUTIIUM_PORT ROUTIIUM_BIND_ADDR ROUTIIUM_ROUTER_URL ROUTIIUM_SLED_PATH ROUTIIUM_BASE ROUTIIUM_BASE_URL

detect_compose() {
  if command -v docker >/dev/null 2>&1; then
    if docker compose version >/dev/null 2>&1; then
      echo "docker compose"
      return
    fi
  fi
  if command -v docker-compose >/dev/null 2>&1; then
    echo "docker-compose"
    return
  fi
  echo "Error: docker compose (v2) or docker-compose (v1) is required." >&2
  exit 1
}

COMPOSE=${COMPOSE:-$(detect_compose)}
COMPOSE_FILE=${COMPOSE_FILE:-docker-compose.e2e.yml}
TESTER_CONTAINER_NAME=${TESTER_CONTAINER_NAME:-edurouter_e2e_tester_$$}

remove_tester_container() {
  docker rm -f "${TESTER_CONTAINER_NAME}" >/dev/null 2>&1 || true
}

cleanup() {
  remove_tester_container
  ${COMPOSE} -f "${COMPOSE_FILE}" down --remove-orphans >/dev/null 2>&1 || true
}

trap cleanup EXIT

echo "Using compose command: ${COMPOSE}"
echo "Building docker images for e2e..."
${COMPOSE} -f "${COMPOSE_FILE}" build edurouter routiium tester

echo "Starting edurouter and routiium..."
${COMPOSE} -f "${COMPOSE_FILE}" up -d edurouter routiium

echo "Waiting for routiium at ${ROUTIIUM_HOST_URL}..."
wait_for_http "${ROUTIIUM_HOST_URL%/}/status" 90
ensure_routiium_token

echo "Running tester workload..."
remove_tester_container
set +e
${COMPOSE} -f "${COMPOSE_FILE}" run --name "${TESTER_CONTAINER_NAME}" tester
TESTER_EXIT_CODE=$?
set -e

if docker cp "${TESTER_CONTAINER_NAME}:/e2e/perf_report.json" "${PROJECT_ROOT}/e2e/perf_report.json" >/dev/null 2>&1; then
  echo "Saved perf report to e2e/perf_report.json"
else
  echo "Warning: unable to copy perf report from tester container" >&2
fi
remove_tester_container

if [[ ${TESTER_EXIT_CODE} -ne 0 ]]; then
  echo "Tester workload failed with status ${TESTER_EXIT_CODE}"
  exit "${TESTER_EXIT_CODE}"
fi

run_llm_characterization

echo "E2E test completed. Containers will be stopped."
