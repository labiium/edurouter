# Embedding Routing Implementation

This document describes how the embedding-aware routing system described in `EMBEDDINGS.md` is wired into EduRouter.

## Runtime components

1. **Config surface** – `RouterConfig` now exposes an optional `EmbeddingConfig`. The env knobs are:
   - `ROUTER_EMBEDDINGS_ENABLED` toggles the feature.
   - `ROUTER_CANONICAL_TASKS` points to the canonical bank (`configs/canonical_tasks.json`).
   - `ROUTER_EMBEDDINGS_PROVIDER` selects `fastembed` (ONNX Runtime) or `hashed` (deterministic fallback used in tests).
   - `ROUTER_EMBEDDINGS_FASTEMBED_MODEL`, `ROUTER_EMBEDDINGS_TOP_K`, and `ROUTER_EMBEDDINGS_CACHE_MS` tune model choice, voting window, and cache TTL.

2. **EmbeddingRuntime (`src/embedding.rs`)**
   - Loads canonical tasks at startup, computes embeddings (via the chosen provider), normalizes them, and keeps them in memory.
   - Maintains a `moka` cache keyed by the hash of the latest user summary so repeated prompts avoid recomputing embeddings.
   - Provides `select(&RouteRequest)` which embeds the current summary, finds the top-k canonical tasks, and returns a `CanonicalSelection` (preferred model + contributing IDs + score), filtering out weak matches (<0.2 similarity).
   - Providers:
     - `FastEmbedBackend`: wraps `fastembed` and runs inference inside `spawn_blocking`; works on CPU and GPU depending on ONNX Runtime support.
     - `HashingBackend`: lightweight deterministic embedding used in tests/CI.

3. **Canonical task bank** – `configs/canonical_tasks.json` ships with representative prompts mapping to catalog models. Operators can edit this file and set `ROUTER_CANONICAL_TASKS` to a custom path.

## RouterEngine integration

- `RouterEngine` holds an `Option<Arc<EmbeddingRuntime>>`. During `plan`:
  1. If embeddings are enabled and the request provides a `conversation.summary` (or override summary), the runtime generates/selects a canonical match.
  2. The resulting hash is folded into `CacheKey::derive`, ensuring canonical-specific plans are cached separately.
  3. The matched model receives a score boost inside `score_candidates`, biasing selection toward the canonical preference while still respecting policy constraints and stickiness.
  4. `PlanAssembly` carries the optional `CanonicalSelection` so the resulting `RoutePlan` embeds a `canonical` section (`ids`, `model`, `score`).
  5. Response headers (`X-Canonical-Model`, `X-Canonical-Ids`, `X-Canonical-Score`) surface the decision, and `X-Route-Why` emits `canonical:model_id` when applicable.

## Testing

- All existing contract tests now run with the hashed provider via `EmbeddingConfig` so every `/route/plan` call exercises the embedding pipeline.
- `canonical_hint_routing_emits_headers` ensures canonical matches reroute the request, emit telemetry headers, and populate the `canonical` block in the plan body.
- Unit-level behavior (normalization, provider fallbacks) is covered indirectly via integration tests without pulling large ONNX models during CI.

## Performance considerations

- Embedding vectors are normalized once at startup (canonical) and cached per summary for `ROUTER_EMBEDDINGS_CACHE_MS` (default 5 minutes).
- Canonical matching performs a simple dot-product against ~100 vectors and aggregates scores in-memory (<1 ms).
- Choosing `fastembed` keeps inference local; hashed mode avoids model downloads for environments without GPU/ONNX support.

This implementation keeps the routing hot path non-blocking, adds observability for canonical decisions, and allows operators to tune or disable the feature entirely via environment variables.
