#!/usr/bin/env bash
set -euo pipefail

COMPOSE=${COMPOSE:-docker compose}
COMPOSE_FILE=${COMPOSE_FILE:-docker-compose.e2e.yml}

cleanup() {
  ${COMPOSE} -f "${COMPOSE_FILE}" down --remove-orphans >/dev/null 2>&1 || true
}

trap cleanup EXIT

echo "Building edurouter image for e2e..."
${COMPOSE} -f "${COMPOSE_FILE}" build edurouter

echo "Starting edurouter and routiium..."
${COMPOSE} -f "${COMPOSE_FILE}" up -d edurouter routiium

echo "Running tester workload..."
${COMPOSE} -f "${COMPOSE_FILE}" run --rm tester bash -c "
  pip install --quiet --no-cache-dir -r /e2e/requirements.txt &&
  python /e2e/runner.py
"

echo "E2E test completed. Containers will be stopped."
