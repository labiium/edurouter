# EduRouter API Reference

EduRouter implements the Routiium Router Schema 1.1 surface. Use this document to integrate Routiium (or any compatible client) with the router service hosted in this repository.

- **Base URL (default):** `http://localhost:9099`
- **Content Type:** `application/json; charset=utf-8`
- **Authentication:** None by default. Front the service with your own gateway/mTLS/network ACLs, especially for admin routes.
- **Character Encoding:** UTF-8.

## Common Conventions

- Timestamps use RFC 3339 / ISO 8601 strings with UTC offsets, e.g., `2024-05-12T17:14:01Z`.
- Currency fields use micro-units (1e-6 of the currency) to match Routiium's pricing math.
- Optional objects may be omitted or set to `null`.
- Errors always return JSON bodies of the form `{ "error": "<slug>", "message": "details" }`.

## Type Reference

### RouteRequest

| Field | Type | Required | Description |
| ----- | ---- | -------- | ----------- |
| `schema_version` | string | optional | Protocol version (`"1.1"` if omitted). |
| `request_id` | string | required | Unique request identifier for tracing. |
| `trace` | object | optional | `{ "traceparent": string, "tracestate": string }`. |
| `alias` | string | required | Logical alias defined in the policy (`configs/policy.json`). |
| `api` | enum | required | One of `"responses"` or `"chat"`; indicates the downstream API surface expected. |
| `privacy_mode` | enum | required | `"features_only"`, `"summary"`, or `"full"`; controls how much content is sent to the router. |
| `content_attestation` | object | optional | `{ "included": "none"|"summary"|"full" }` describing what content was inspected. |
| `caps` | array[string] | optional | Extra capability hints (e.g., `"vision"`, `"json"`). |
| `stream` | bool | required | Whether the caller expects streaming. |
| `params` | object | optional | Free-form JSON for extra toggles (e.g., `{"json_mode": true}`). |
| `targets` | object | optional | Latency and throughput targets (`{ "p95_latency_ms": u32, ... }`). |
| `budget` | object | optional | `{ "amount_micro": u64, "currency": string }`. Plans exceeding the budget are rejected. |
| `estimates` | object | optional | `{ "prompt_tokens": u32, "max_output_tokens": u32, "tokenizer_id": string }`. |
| `conversation` | object | optional | `{ "turns": u16, "system_fingerprint": string, ... }` used for prompt-cache heuristics. |
| `org` | object | optional | `{ "tenant": string, "project": string, "role": string }`; influences overlays/stickiness. |
| `geo` | object | optional | `{ "region": string }` for region-aware routing. |
| `tools` | array[object] | optional | Each tool hint contains `name` and optional `json_schema_hash`. |
| `overrides` | object | optional | Free-form overrides such as `plan_token` (stickiness) or `teacher_boost`. |

### RoutePlan

| Field | Type | Description |
| ----- | ---- | ----------- |
| `schema_version` | string | Echoes the request version. |
| `route_id` | string | Unique identifier for the generated plan (appears in headers). |
| `upstream` | object | `{ "base_url": string, "mode": "responses"|"chat", "model_id": string, "auth_env": string?, "headers": object }`. |
| `limits` | object | Optional `{ "max_input_tokens": u32, "max_output_tokens": u32, "timeout_ms": u32 }`. |
| `prompt_overlays` | object | Optional overlay payload and fingerprint metadata. |
| `hints` | object | Optional `{ "tier": string?, "est_cost_micro": u64?, "currency": string?, "est_latency_ms": u32?, "provider": string? }`. |
| `fallbacks` | array | Optional list of alternate upstreams (same shape as `upstream` plus `reason` and `penalty`). |
| `cache` | object | `{ "ttl_ms": u32?, "etag": string?, "valid_until": string?, "freeze_key": string? }`. |
| `stickiness` | object | `{ "plan_token": string?, "max_turns": u8?, "expires_at": string? }` used by Routiium for conversational routing. |
| `policy` | object | `{ "revision": string?, "id": string?, "explain": string? }`. |
| `content_used` | enum | `"none"`, `"summary"`, or `"full"`; indicates how much request content the router consumed. |

### RouteFeedback

| Field | Type | Required | Description |
| ----- | ---- | -------- | ----------- |
| `route_id` | string | required | Identifier returned by `/route/plan`. |
| `model_id` | string | required | Actual upstream model used. |
| `success` | bool | required | Whether the request succeeded upstream. |
| `duration_ms` | u32 | required | Total time spent calling the upstream. |
| `usage` | object | optional | `{ "prompt_tokens": u32, "completion_tokens": u32, "cached_tokens": u32, "reasoning_tokens": u32 }`. |
| `status_code` | u16 | required | HTTP status from the upstream provider. |
| `actual_cost_micro` | u64 | optional | Final calculated cost in micro-units. |
| `currency` | string | optional | Currency code (e.g., `"USD"`). |
| `upstream_error_code` | string | optional | Provider-specific error identifier. |
| `rl_applied` | bool | optional | Indicates whether downstream rate limiting was applied. |
| `cache_hit` | bool | optional | Whether prompt-cache was used. |

### RouterStats

Response structure for `GET /stats`:

```json
{
  "policy_revision": "rev-2024-05-01",
  "catalog_revision": "cat-2024-05-01",
  "total_requests": 12345,
  "cache_hit_ratio": 0.78,
  "model_share": {
    "gpt-4o": 4567,
    "claude-3": 1200
  },
  "error_rate": 0.01
}
```

## Endpoints

### POST /route/plan

Generate a plan for a specific alias.

- **Headers:** `Content-Type: application/json`
- **Body:** `RouteRequest`

Example request:

```bash
curl -s http://localhost:9099/route/plan \
  -H "Content-Type: application/json" \
  -d '{
        "schema_version": "1.1",
        "request_id": "demo-1",
        "alias": "gpt-4o-educator",
        "api": "responses",
        "privacy_mode": "features_only",
        "stream": true,
        "caps": ["vision", "tools"],
        "estimates": {"prompt_tokens": 600, "max_output_tokens": 300},
        "org": {"tenant": "district-12", "role": "teacher"},
        "geo": {"region": "us"}
      }'
```

Example response:

```json
{
  "schema_version": "1.1",
  "route_id": "c21d7c8d-36b2-4ef0-a3f7-7a7fa9d6d1a0",
  "upstream": {
    "base_url": "https://api.openai.com/v1",
    "mode": "responses",
    "model_id": "gpt-4o-mini",
    "auth_env": "OPENAI_API_KEY",
    "headers": {
      "x-edu-tier": "teacher"
    }
  },
  "limits": {
    "max_input_tokens": 8192,
    "max_output_tokens": 2048,
    "timeout_ms": 60000
  },
  "hints": {
    "tier": "tier:T1",
    "est_cost_micro": 25000,
    "currency": "USD",
    "est_latency_ms": 900,
    "provider": "openai"
  },
  "fallbacks": [
    {
      "base_url": "https://api.anthropic.com/v1",
      "mode": "responses",
      "model_id": "claude-3-sonnet",
      "reason": "alternate",
      "penalty": 0.1
    }
  ],
  "cache": {
    "ttl_ms": 15000,
    "etag": "rev-2024-05-01"
  },
  "stickiness": {
    "plan_token": "eyJ0b2tlbiI6ICJ...",
    "max_turns": 4,
    "expires_at": "2024-05-12T17:25:02Z"
  },
  "policy": {
    "revision": "rev-2024-05-01",
    "id": "default",
    "explain": "score=0.87 cost=25000u latency=900ms"
  },
  "content_used": "none"
}
```

Response headers:

| Header | Description |
| ------ | ----------- |
| `Router-Schema` | Schema version of the response (`1.1`). |
| `Router-Latency` | Time EduRouter spent planning (e.g., `4ms`). |
| `Config-Revision` | Policy revision used for scoring. |
| `Catalog-Revision` | Catalog revision used. |
| `X-Route-Cache` | `hit`, `miss`, or `stale` depending on cache usage. |
| `X-Resolved-Model` | Primary model ID selected. |
| `X-Route-Id` | Same as `route_id` in the JSON body. |
| `X-Route-Tier` | Present when `hints.tier` is set. |
| `X-Content-Used` | Indicates how much request content the router consumed (`none|summary|full`). |

### POST /route/feedback

Submit execution results so EduRouter can update rolling health metrics.

- **Headers:** `Content-Type: application/json`
- **Body:** `RouteFeedback`
- **Success:** `204 No Content`

Example:

```bash
curl -X POST http://localhost:9099/route/feedback \
  -H "Content-Type: application/json" \
  -d '{
        "route_id": "c21d7c8d-36b2-4ef0-a3f7-7a7fa9d6d1a0",
        "model_id": "gpt-4o-mini",
        "success": true,
        "duration_ms": 1100,
        "usage": {"prompt_tokens": 620, "completion_tokens": 200},
        "status_code": 200,
        "actual_cost_micro": 23000,
        "currency": "USD"
      }'
```

### GET /catalog/models

Returns the live catalog document loaded from `ROUTER_CATALOG_PATH`.

- **Success:** `200 OK`
- **Body:** `CatalogDocument` (see `src/types.rs`).

### GET /policy

Returns the active policy document loaded from `ROUTER_POLICY_PATH`.

- **Success:** `200 OK`
- **Body:** `PolicyDocument`.

### GET /stats

Provides aggregate usage statistics.

- **Success:** `200 OK`
- **Body:** `RouterStats` (see example above).

### GET /healthz

Simple health probe for load balancers.

- **Success:** `200 OK`
- **Body:**

```json
{
  "status": "ok",
  "policy_revision": "rev-2024-05-01",
  "catalog_revision": "cat-2024-05-01",
  "timestamp": "2024-05-12T17:14:01Z"
}
```

## Admin Endpoints

These endpoints modify in-memory state. Restrict access.

### POST /admin/policy

Replace the active policy document.

```bash
curl -X POST http://localhost:9099/admin/policy \
  -H "Content-Type: application/json" \
  -d @configs/policy.json
```

Response: `204 No Content` on success. Plans cached before the reload are invalidated automatically.

### POST /admin/catalog

Replace the active catalog document.

```bash
curl -X POST http://localhost:9099/admin/catalog \
  -H "Content-Type: application/json" \
  -d @configs/catalog.json
```

Response: `204 No Content`. Policy is recompiled to ensure model indices stay in sync.

### POST /admin/overlays/reload

Reload overlay files from `ROUTER_OVERLAY_DIR` without uploading JSON.

```bash
curl -X POST http://localhost:9099/admin/overlays/reload
```

Response: `204 No Content`.

## Error Codes

| HTTP Status | `error` slug | When it occurs |
| ----------- | ------------ | -------------- |
| `400 Bad Request` | `unknown_alias` | Alias not found in the policy document. |
| `403 Forbidden` | `invalid_approval` | Stickiness token cannot be verified or is expired. |
| `500 Internal Server Error` | `unknown_model`, `planning_error`, `io_error`, `internal_error` | Any other issue while compiling policy/catalog, scoring, or reading files. |

Retries are safe for `500` errors. For `400` errors you must fix the request (e.g., use a valid alias). For `403`, obtain a fresh plan token by omitting the stale `plan_token` override.

## Integration Tips

1. Always include `schema_version` and `request_id` so logs stay correlated.
2. Cache stickiness tokens (`plan.stickiness.plan_token`) on the client and send them via `overrides.plan_token` for multi-turn chats.
3. Monitor `X-Route-Cache` headers to verify whether responses are being reused; a sudden drop to `miss` may indicate policy/catalog churn.
4. Call `/route/feedback` even on failures so the router can degrade unhealthy models quickly.
5. Automate policy/catalog reloads as part of your CI/CD pipeline to keep revisions in sync across router pods.
