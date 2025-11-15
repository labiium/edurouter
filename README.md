# EduRouter (Routiium-Compatible Router)

EduRouter is a lightweight Routiium-compatible routing surface that evaluates policy, catalog, and runtime health data to produce Schema 1.1 `RoutePlan` objects for the Routiium proxy. It focuses on education-focused deployments where managed routing, deterministic stickiness, and transparent observability matter more than tightly coupling policy to a single upstream provider.

This repository contains the Actix-web service that accepts `/route/plan` and related calls, a caching/scoring engine written in Rust, and configuration helpers for shipping a drop-in router alongside Routiium. If you're running Routiium with policy-aware routing enabled (`ROUTIIUM_ROUTER_URL`), this service resolves aliases such as `openai-multimodal` into concrete upstreams, headers, prompts, and stickiness metadata.

## Table of Contents

1. [Key Features](#key-features)
2. [Component Overview](#component-overview)
3. [Quick Start](#quick-start)
4. [Configuration](#configuration)
5. [Request Lifecycle](#request-lifecycle)
6. [Operations & Observability](#operations--observability)
7. [Repository Layout](#repository-layout)
8. [Development Workflow](#development-workflow)
9. [Additional Resources](#additional-resources)

## Key Features

- **Routiium-compliant API** - Implements the Router Schema 1.1 contract (`/route/plan`, `/route/feedback`, `/catalog/models`, `/policy`) so Routiium can consume plans without custom glue.
- **Policy-driven scoring** - Uses `configs/policy.json` to weight cost, latency, model health, and context-fit for every alias.
- **Plan caching** - Memorizes scoring results in a TTL cache keyed by alias, capability mask, token buckets, and region so repeat requests resolve in microseconds.
- **Stickiness tokens** - Issues HMAC-signed plan tokens so Routiium can keep conversations on the same upstream until an expiry/max-turn window is hit.
- **Prompt overlays** - Loads overlays from disk and embeds fingerprints in the plan so downstream services know which system prompt to apply.
- **Health-aware routing** - Folds `/route/feedback` into rolling latency/error/tokens-per-second stats to automatically deprioritize unhealthy models.

## Component Overview

| Area | Description | Key Files |
| ---- | ----------- | --------- |
| HTTP surface | Actix-web server with CORS, logging, and handlers for plans, feedback, stats, and reloads. | `src/main.rs`, `src/api.rs` |
| Router engine | Scores candidates, builds `RoutePlan`s, attaches overlays/stickiness, and records metrics. | `src/engine.rs` |
| Schema types | Shared structs for requests, plans, policy, catalog, and feedback. | `src/types.rs` |
| Config loading | Reads env vars, policy, catalog, and overlay directories. | `src/config.rs` |
| Cache & stickiness | TTL plan cache plus HMAC token issuer/validator. | `src/cache.rs`, `src/stickiness.rs` |
| Health tracking | Aggregates `RouteFeedback` into per-model latency/error stats. | `src/health.rs` |
| Error handling | Consistent JSON error envelopes surfaced via Actix `ResponseError`. | `src/errors.rs` |

Refer to [API_REFERENCE.md](API_REFERENCE.md) for exhaustive request/response documentation.

## Quick Start

### 1. Clone & prepare configs

```bash
git clone https://github.com/labiium/edurouter.git
cd edurouter
# Edit configs/policy.json, configs/catalog.json, and the overlays under configs/overlays/
# to match your upstreams, tiers, and prompts.
```

Update the policy and catalog to match your upstreams, capabilities, and tiers.

### 2. Run locally

```bash
export ROUTER_BIND=0.0.0.0:9099
export ROUTER_POLICY_PATH=./configs/policy.json
export ROUTER_CATALOG_PATH=./configs/catalog.json
export ROUTER_OVERLAY_DIR=./configs/overlays
export ROUTER_CACHE_TTL_MS=15000

cargo run --release
```

Smoke tests:

```bash
curl http://localhost:9099/healthz
curl -s http://localhost:9099/route/plan \
  -H "Content-Type: application/json" \
  -d '{
        "schema_version": "1.1",
        "request_id": "demo-1",
        "alias": "openai-multimodal",
        "api": "responses",
        "privacy_mode": "features_only",
        "stream": true
      }'
```

### 3. Deploy via Docker

```bash
docker build -t edurouter .
docker run --rm \
  -p 9099:9099 \
  -v $PWD/configs:/app/configs \
  -e ROUTER_POLICY_PATH=/app/configs/policy.json \
  -e ROUTER_CATALOG_PATH=/app/configs/catalog.json \
  -e ROUTER_OVERLAY_DIR=/app/configs/overlays \
  edurouter
```

Point Routiium at the router by setting `ROUTIIUM_ROUTER_URL=http://router:9099`.

## Configuration

### Environment Variables

| Variable | Default | Purpose |
| -------- | ------- | ------- |
| `ROUTER_BIND` | `0.0.0.0:9099` | Socket for the Actix server. |
| `ROUTER_WORKERS` | physical CPU count | Number of worker threads. |
| `ROUTER_POLICY_PATH` | `./configs/policy.json` | Policy document (JSON or YAML). |
| `ROUTER_CATALOG_PATH` | `./configs/catalog.json` | Catalog document describing every upstream model. |
| `ROUTER_OVERLAY_DIR` | `./configs/overlays` | Directory of overlay text snippets keyed by filename stem. |
| `ROUTER_CACHE_TTL_MS` | `15000` | TTL (ms) for cached plans and default stickiness window. |
| `ROUTER_STICKY_SECRET` | random dev secret | Base64-encoded HMAC secret shared across router pods. |

These vars are read once at startup; policy, catalog, and overlays can then be hot-reloaded via admin endpoints.

### Embedding-Aware Routing

| Variable | Default | Purpose |
| -------- | ------- | ------- |
| `ROUTER_EMBEDDINGS_ENABLED` | `0` | Enable canonical-task similarity routing when set to `1`. |
| `ROUTER_CANONICAL_TASKS` | `./configs/canonical_tasks.json` | Path to the canonical task bank. |
| `ROUTER_EMBEDDINGS_PROVIDER` | `fastembed` | Embedding backend (`fastembed` for ONNX inference, `hashed` for deterministic tests). |
| `ROUTER_EMBEDDINGS_FASTEMBED_MODEL` | `bge-small-en-v1.5` | FastEmbed model slug or model code. |
| `ROUTER_EMBEDDINGS_TOP_K` | `3` | Number of canonical tasks considered per request. |
| `ROUTER_EMBEDDINGS_CACHE_MS` | `300000` | TTL for per-text embedding cache. |
| `ROUTER_EMBEDDINGS_ALLOW_HASHED` | `0` | Must be `1` to use the `hashed` provider (intended only for CI/tests). |

FastEmbed downloads the model once at startup; hashed mode is a lightweight deterministic fallback useful for CI.

### Policy Document (summary)

- `revision` / `schema_version` - surfaced in headers for traceability.
- `weights` - floats for `cost`, `latency`, `health`, `context`, plus optional `tier_bonus`.
- `defaults` - `cost_norm_micro`, `latency_ms`, `timeout_ms`, `max_output_tokens`, and stickiness config (`window_ms`, `max_turns`).
- `aliases` - alias -> `{ candidates, require_caps, allowed_regions }`.
- `overlay_map` / `overlay_defaults` - map alias or org role to overlay IDs found inside `ROUTER_OVERLAY_DIR`.

### Catalog Document (summary)

Each model entry should define:

- `id`, `provider`, optional `region` list, optional `aliases`, and `policy_tags`.
- `capabilities` - modalities, context window, tool/json/prompt-cache support flags.
- `limits` - TPS/RPM hints.
- `cost` - micro currency pricing for input/output/cached tokens.
- `slos` - target and recent latency/error metrics.
- `metadata` - `base_url`, `mode` (`responses` or `chat`), optional `auth_env`, and static `headers`.

## Request Lifecycle

1. **Plan request** - Routiium sends `/route/plan` with alias, capability hints, region, budgets, and optional overrides (stickiness token or teacher boost).
2. **Candidate filtering** - `RouterEngine` filters catalog entries by capability mask, allowed regions, context window, status, and budgets.
3. **Scoring & selection** - Candidates are scored using policy weights, health snapshot, and cost/latency estimates; the top candidate becomes primary.
4. **Fallback construction** - Up to three alternates are attached so Routiium can retry without another plan call.
5. **Plan caching** - The resulting `RoutePlan` is cached for `ROUTER_CACHE_TTL_MS` and reused if equivalent requests arrive.
6. **Stickiness tokens** - Plans include signed tokens so future requests can stick to the same upstream; cache validity is aligned to the token expiration.
7. **Feedback loop** - Routiium calls `/route/feedback`; EduRouter updates health stats, influencing future scoring without restarts.

## Operations & Observability

- **Health probe** - `GET /healthz` returns `{ status, policy_revision, catalog_revision, timestamp }`.
- **Router stats** - `GET /stats` exposes cache hit ratio, per-model share, and total requests.
- **Catalog/policy introspection** - `GET /catalog/models` and `GET /policy` return the live documents Routiium consumes; `GET /capabilities` advertises privacy/stickiness support knobs.
- **Admin reloads** -
  - `POST /admin/policy` (body: policy document) reloads aliases/weights.
  - `POST /admin/catalog` reloads model metadata.
  - `POST /admin/overlays/reload` refreshes overlay files from disk.
- **Plan response headers** - Each `/route/plan` response includes `Router-Schema`, `Router-Latency`, `Config-Revision`, `Catalog-Revision`, `X-Route-Cache`, `X-Route-Id`, `X-Resolved-Model`, `X-Route-Tier`, `X-Route-Provider`, `X-Policy-Rev`, `X-Content-Used`, and optional context headers such as `X-Route-Why`, `traceparent`, and `tracestate`.
- **Error envelope** - Failures return typed JSON `{"schema_version":"1.1","code":"ALIAS_UNKNOWN","message":"...","request_id":"...","policy_rev":"...","retry_hint_ms":60000}` with HTTP status mapped to the code (404/409/403/402/503/502/400/409/500).

Comprehensive payload, header, and error details live in [API_REFERENCE.md](API_REFERENCE.md).

## Repository Layout

```
+-- Cargo.toml          # crate metadata and dependencies
+-- Dockerfile          # container build
+-- README.md           # overview (this file)
+-- API_REFERENCE.md    # endpoint documentation
+-- src
    +-- main.rs         # Actix entry point, logging, CORS, worker config
    +-- api.rs          # HTTP handlers and response shaping
    +-- config.rs       # env + file loading
    +-- engine.rs       # planner, caching, stickiness, overlays
    +-- cache.rs        # plan cache wrapper
    +-- stickiness.rs   # HMAC token issue/verify helpers
    +-- health.rs       # feedback-driven health model
    +-- types.rs        # shared structs for schema 1.1
```

## Development Workflow

1. **Format & lint**
   ```bash
   cargo fmt
   cargo clippy --all-targets --all-features
   ```
2. **Run tests**
   ```bash
   cargo test
   ```
3. **Iterate on configs**
   - Keep editable copies under `configs/`.
   - Use the admin endpoints to reload policy/catalog/overlays without restarting.
4. **Observe logs**
   - Set `RUST_LOG=router=debug,actix_web=info` to see scoring decisions, cache hits, and stickiness events.

## Additional Resources

- [API_REFERENCE.md](API_REFERENCE.md) - Exhaustive HTTP contract, curl snippets, and schema notes.
- [Routiium](https://github.com/labiium/routiium) - Upstream proxy that consumes this router via `ROUTIIUM_ROUTER_URL`.
- Example configs under `configs/` - Baseline policy, catalog, and overlay definitions.
- License: MIT (see `LICENSE`).

## Docker E2E Harness

The repo ships a Docker-based end-to-end harness that stands up EduRouter, builds the Routiium proxy from a local checkout, and drives a configurable workload to characterize plan latency and cache behavior. Requirements:

1. Docker + `docker compose`
2. A Routiium source tree (either cloned next to this repo or pointed to via `ROUTIIUM_SOURCE_DIR`).

Run the harness:

```bash
./scripts/run_e2e.sh
```

What it does:

- Builds the local EduRouter image and starts it alongside the Routiium container using `docker-compose.e2e.yml`.
- Runs `e2e/runner.py` in an ephemeral Python container. The runner sends plan requests, validates headers, and records latency/cache metrics.
- Waits for Routiium to come online, mints a short-lived API key via `/keys/generate`, and shares it with follow-up checks.
- Writes a JSON report to `e2e/perf_report.json` summarizing min/avg/p95 latency and cache hit ratios.
- Performs a multimodal chat-completions sanity check through Routiium using `python_tests/test_openai_models.py` and saves the transcript + usage data to `e2e/llm_characterization.json`.
- Verifies embedding-aware routing by calling `python_tests/test_embedding_routing.py`, ensuring canonical tasks steer traffic to the expected model.

Environment variables you can override:

| Variable | Default | Description |
|----------|---------|-------------|
| `COMPOSE` | `docker compose` | Compose command used by the script. |
| `ROUTIIUM_SOURCE_DIR` | `../routiium` (relative to repo root) | Path to the Routiium codebase that will be built into the `routiium:local` image. Must contain `Cargo.toml`. |
| `ROUTIIUM_DOCKERFILE` | `Dockerfile` | Override the Dockerfile used when building Routiium. |
| `SAMPLE_REQUESTS` | `50` | Number of plan requests generated by the runner. |
| `CONCURRENCY` | `4` | Concurrent workers issuing requests. |
| `OUTPUT_PATH` | `/e2e/perf_report.json` | Where the runner writes the JSON report. |
| `RUN_LLM_CHARACTERIZATION` | `1` | Disable (`0`) to skip the post-run multimodal check. |
| `LLM_SAMPLES` | `1` | Requests per model for `python_tests/test_openai_models.py`. |
| `LLM_OUTPUT_PATH` | `e2e/llm_characterization.json` | Location of the LLM characterization artifact. |
| `ROUTIIUM_PORT` | `8088` | Host + container port exposed for Routiium. |
| `ROUTIIUM_BASE` | `http://localhost:8088` | Base URL shared with helper scripts (e.g., key generator). |
| `ROUTIIUM_REUSE_API_KEY` | `0` | Set to `1` to keep using a manually supplied `ROUTIIUM_API_KEY` instead of minting a fresh token per run. |

Example with custom load:

```bash
SAMPLE_REQUESTS=200 CONCURRENCY=16 ./scripts/run_e2e.sh
```

The script automatically tears down the Docker stack after the test finishes.

## Terminal Chat Demo

Want to see EduRouter planning decisions while chatting with a real OpenAI model? Use the terminal helper:

```bash
docker compose -f docker-compose.dev.yml up -d  # starts router+routiium for dev
# (optional) pre-generate a Routiium key; the chat script can also auto-generate one.
export ROUTIIUM_API_KEY="$(curl -s -X POST http://localhost:8088/keys/generate \
  -H 'Content-Type: application/json' -d '{\"label\":\"chat-demo\",\"ttl_seconds\":86400}' | jq -r '.token')"
python python_tests/chat_router_demo.py
```

It will:

- Load `.env` (so `OPENAI_API_KEY`, `ROUTER_URL`, etc., can live there).
- Request a plan for the configured alias (defaults to `openai-multimodal`).
- Call OpenAI using the plan’s upstream fields.
- Print the router tier/cache metadata plus the model’s reply.

`Ctrl+C` exits cleanly. Override defaults with `ROUTER_URL`, `ROUTER_ALIAS`, `ROUTER_CAPS`, or `ROUTER_PRIVACY`. The script prefers managed mode: if `ROUTIIUM_API_KEY` is present (or auto-generated via `ROUTIIUM_AUTO_KEY=1`), all traffic flows through Routiium, so only the server needs the upstream OpenAI key. If no Routiium key is available it falls back to calling OpenAI directly using `OPENAI_API_KEY`.

## OpenAI Live Characterization

If you want to run a real multimodal sanity check through Routiium (or straight to OpenAI) using the Chat Completions surface, use `python_tests/test_openai_models.py`. Requirements:

1. Set `ROUTIIUM_API_KEY` (preferred) or `OPENAI_API_KEY` in your shell/`.env`.
2. Install the OpenAI Python SDK (`pip install openai` or `uv pip install openai`).

Run:

```bash
ROUTIIUM_API_KEY=sk-local... ROUTIIUM_BASE_URL=http://localhost:8088/v1 \
  python python_tests/test_openai_models.py --samples 2
# or hit OpenAI directly
OPENAI_API_KEY=sk-... python python_tests/test_openai_models.py --samples 2
```

This script:

- Sends a small multimodal prompt (text + inline PNG) to each of `gpt-4.1-nano`, `gpt-5-nano`, and `gpt-5-mini`.
- Captures latency, token usage, and estimated USD cost using the published price tiers you provided.
- Writes a JSON report (`openai_model_characterization.json` by default) so you can keep artifacts from the run.

Errors (HTTP or policy) are surfaced immediately; rerun once your account has access to the specified models.

## Key Generator CLI

Need a one-off Routiium key for local tests without hand-crafting curl commands? Use `python_tests/generate_api_key.py`:

```bash
python python_tests/generate_api_key.py --base-url http://localhost:8088 --label demo --ttl-seconds 3600
# machine-readable output
python python_tests/generate_api_key.py --base-url http://localhost:8088 --json
```

The helper loads `.env`, accepts optional `--scope`, `--ttl-seconds`, or `--expires-at`, and prints either a friendly summary or raw JSON (when `--json` is provided). The e2e harness automatically calls this helper to mint a short-lived key before it runs characterization tests.
