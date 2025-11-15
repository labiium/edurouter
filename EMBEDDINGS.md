# Embedding-Aware Routing Plan

This document proposes a latency-aware design for augmenting EduRouter’s routing engine with embedding-powered task classification. The goal is to examine the latest user prompt, compare it against a canonical “task family” set, and bias routing toward the model that historically performs best for that family, while integrating cleanly with existing alias/policy logic.

---

## 1. Objectives

1. **Task similarity routing:** Embed the most recent user message, find the top-k closest canonical tasks, and use their preferred models (weighted by similarity and importance) to bias model selection.
2. **Minimal latency impact:** Only run embeddings when necessary, cache aggressively, and keep the critical path hot.
3. **Seamless integration:** Respect existing alias/policy semantics, `RoutePlan` cache, stickiness, overlays, and fallbacks.
4. **Operational simplicity:** Provide tooling to build/maintain the canonical task set (manual curation, LLM-assisted generation) and ship observability hooks to tune behavior.

---

## 2. Canonical Task Bank

### 2.1 Structure

Create `configs/canonical_tasks.json` with entries like:

```json
[
  {
    "id": "math_proof_step",
    "text": "Explain each step when proving a high-school algebra identity.",
    "preferred_model": "gpt-5-mini",
    "weight": 1.2,
    "tags": ["math", "reasoning"]
  }
]
```

- `id`: stable identifier.
- `text`: canonical prompt description.
- `preferred_model`: upstream model ID or alias.
- `weight`: optional multiplier to favor high-impact tasks. During scoring we multiply similarity by this weight, so business-critical prompts override casual traffic even if similarity is slightly lower.
- `tags`: optional metadata for policy introspection. They enable future routing rules or analytics (e.g., force educational overlays when the canonical tag includes `classroom`).
- `embedding`: **not stored** in the JSON. Embeddings are computed at startup using the selected provider so the canonical file stays provider-agnostic. The loader can optionally persist a sidecar cache (e.g., `.cache/canonical_embeddings.bin`) keyed by `(provider, model, canonical_id)` to avoid recomputation after restarts, but the source-of-truth JSON remains clean.

### 2.2 Building the Canonical Set

Options:
- **Manual curation:** Domain experts supply ~100 representative prompts spanning desired workloads.
- **LLM-assisted generation:** Use a helper script (`python_tests/generate_canonical_tasks.py`) that prompts GPT-4.1 to produce representative tasks per capability (math, coding, drafting). Human review refines the set.
- **Data mining:** Sample anonymized conversation histories, cluster them via embeddings (e.g., k-means on `text-embedding-3-large`), and pick cluster centroids as canonical tasks. Store cluster insights as `tags` (`"math"`, `"support"`, `"code"`, etc.) so policy/analytics can report coverage.

The script should:
1. Dump tasks to JSON (no embeddings).
2. Optionally generate embeddings for validation/analysis, but runtime code remains responsible for producing the vectors used in routing.
3. Optionally compute per-task metrics (success rate, latency) from router logs and write those as `weight` values so runtime scoring can leverage them.

---

## 3. Embedding Workflow in EduRouter

### 3.1 New Module

Add `src/embedding_router.rs` exposing:
- `struct CanonicalTask { id, preferred_model, weight, tags, embedding: Vec<f32> }` where `embedding` is populated at runtime when the loader computes vectors for each entry.
- `struct EmbeddingRouter { tasks: Vec<CanonicalTask>, index: faiss-like structure or simple Vec }`
- `impl EmbeddingRouter { fn from_file(path) -> Result<Self>; fn select(&self, query: &[f32], top_k: usize) -> Vec<ScoredModel>; }`

`ScoredModel` would contain `{ model_id: String, score: f32, canonical_ids: Vec<String> }`.

### 3.2 Embedding Provider

Add a trait + implementation for embedding providers:

```rust
#[async_trait]
pub trait EmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError>;
}
```

Possible implementations:
- **OpenAI `text-embedding-3-large`:** use `reqwest` client already in AppState. Pros: accurate, easy. Cons: adds network hop and cost.
- **Local model (e.g., `qdrant/fastembed`, `ggml`):** add dependency to run in-process (via `fastembed` crate or cbindgen). Pros: avoids network latency. Cons: increases binary size.
- **Deterministic hash backend:** lightweight fallback used in tests/CI so the router can exercise the pipeline without downloading ONNX models. Guarded by `ROUTER_EMBEDDINGS_ALLOW_HASHED=1` and not recommended for production.

For minimal latency, prefer a local embedding server (maybe a sidecar service) and call it over localhost with low timeout. Provide env config:

- `ROUTER_EMBEDDINGS_ENABLED=1`
- `ROUTER_EMBEDDINGS_PROVIDER=openai|http|disabled`
- `ROUTER_EMBEDDINGS_TOP_K=5`
- `ROUTER_CANONICAL_TASKS=configs/canonical_tasks.json`
- `ROUTER_EMBEDDINGS_CACHE_MS=300000` (conversation-level embedding cache TTL)

At startup the `EmbeddingRouter` uses the configured provider to compute embeddings for every canonical entry and keeps them in memory. If the canonical file or provider changes, re-run the loader (or hit a future reload endpoint) so vectors stay in sync.

### 3.3 Integration Points

In `engine.rs` (before cache lookup):
1. Extract last user message:
   ```rust
   let last_user_text = request.conversation.latest_user_text();
   ```
   (Add helper in `types.rs`.)
2. If embeddings enabled and `last_user_text` present:
   - Check conversation-level cache (e.g., `HashMap<ConversationId, CachedEmbedding>` with TTL).
   - Else call `EmbeddingProvider::embed`.
3. Run `EmbeddingRouter::select` to get ranked models.
4. Attach result to request context, e.g.:
   ```rust
   req.overrides.canonical_hint = Some(CanonicalHint {
       model: top_model_id,
       score: top_score,
       canonical_ids: top_ids,
   });
   ```
5. Use that hint during policy evaluation:
   - In `config.rs` policy application, if `overrides.canonical_hint` exists, try to pick the matching upstream first (ensuring capabilities align). If not feasible, fall back to normal alias routes.
6. Include canonical info in plan headers/telemetry:
   - Add `x-canonical-id`, `x-canonical-score`, etc., so downstream analytics know why a model was chosen.
7. Update cache key/freeze key to include `canonical_ids` so plans only reuse when the prompt stays in the same family.

### 3.4 Latency Minimization

- **Cache embeddings per conversation** for `ROUTER_EMBEDDINGS_CACHE_MS` (e.g., 5 minutes). Store in `AppState`.
- Skip embedding when:
  - Request hits plan cache (since the plan already encoded canonical info).
  - Request lacks user text (system-only messages).
  - `ROUTER_EMBEDDINGS_MIN_TOKENS` threshold not met.
- Batch canonical similarity computation: pre-normalize vectors, use dot product for fast top-k (no need for FAISS unless >10k tasks). For ~100 entries, simple linear scan is <1 ms.
- Add tracing spans around embedding + similarity steps to monitor latency.

---

## 4. Policy & Routing Adjustments

- Extend policy schema to recognize `canonical_hint`:
  ```json
  {
    "routes": [
      {
        "when": { "canonical_model": "gpt-5-mini" },
        "target": "gpt-5-mini",
        "priority": 5
      },
      ...
    ]
  }
  ```
- Provide fallback: if canonical model lacks required caps (e.g., user now uses tools), policy continues to evaluate other routes.
- Stickiness: include canonical ID in stickiness context so successive turns stay with that model even if embeddings are skipped.

---

## 5. Libraries & Dependencies

- **Rust crates:**
  - `serde_json`/`serde` (already present) for canonical config parsing.
  - `rust-bert` or `fastembed` if embedding in-process.
  - `ndarray` or `nalgebra` for vector math (optional; simple `Vec<f32>` dot product is enough).
  - `parking_lot` for caching locks (optional).
- **External services:**
  - Optionally add HTTP client for embedding service (reuse `reqwest`).
  - If using OpenAI embeddings, add config for API key / base URL (piggyback on existing env).

---

## 6. Implementation Steps

1. **Config scaffolding**
   - Add new env vars to `README.md` and `config.rs`.
   - Create `configs/canonical_tasks.example.json`.
2. **Canonical loader**
   - Implement `CanonicalTask` parser + `EmbeddingRouter` struct.
   - Add CLI (`cargo run --bin canonical_builder`) to regenerate embeddings using `python_tests/build_canonicals.py`.
3. **Embedding provider**
   - Implement provider trait with OpenAI + HTTP options.
   - Respect timeouts (e.g., 200 ms) and caching.
4. **Planner integration**
   - Hook into `engine.rs` to call embedding logic before cache usage.
   - Modify `RoutePlan` creation to include canonical metadata.
5. **Policy support**
   - Update policy evaluation to consider `canonical_hint`.
   - Add tests in `tests/contract.rs` verifying canonical hints override defaults.
6. **Observability**
   - Add tracing spans + metrics (e.g., `router.embedding.latency_ms`, `router.embedding.cache_hits`).
   - Log canonical IDs for debugging.
7. **Docs**
   - Update `README.md` with new env knobs and architecture summary.
   - Document canonical builder workflow in `EMBEDDINGS.md` (this file) and `python_tests/README.md`.

---

## 7. Testing Strategy

- **Unit tests**
  - `embedding_router.rs`: verify top-k selection and scoring.
  - Policy tests to ensure canonical hints override target selection.
- **Integration tests**
  - Add a test alias with two models; simulate embeddings selecting one vs. the other.
  - Ensure caching works: first request uses embedding, second hit reuses plan without re-embedding.
- **Performance tests**
  - Use `scripts/run_e2e.sh` with embeddings enabled to measure added latency (<5 ms target for canonical lookup).
  - Benchmark worst-case (embedding provider offline) to ensure router falls back gracefully or times out quickly.

---

## 8. Open Questions / Future Enhancements

- **Dynamic canonical updates:** Build an admin endpoint to reload canonical tasks at runtime (`POST /admin/canonicals/reload`).
- **Feedback loop:** Use `/route/feedback` to record model success by canonical ID and auto-adjust weights.
- **Hybrid strategies:** Combine canonical similarity with rule-based caps (e.g., only apply canonical routing for reasoning aliases).
- **Local embedding store:** Consider shipping a tiny embedding sidecar (e.g., Nomic’s `embed-mistral`) to avoid OpenAI dependency.

### 8.1 Library Spotlight: `fastembed-rs`

[`fastembed-rs`](https://crates.io/crates/fastembed) is a production-ready Rust library that aligns well with this plan:

- **Features:** synchronous API (no Tokio), ONNX Runtime backend via `pykeio/ort`, fast HuggingFace tokenizers, and built-in rerankers/image embeddings.
- **Model coverage:** includes popular text embedding models (`BAAI/bge-small-en-v1.5`, `all-MiniLM-L6-v2`, `nomic-embed-text`, multilingual E5, ModernBERT, etc.), sparse embedding models (SPLADE), and image or reranker models.
- **Deployment:** runs on CPU by default; can leverage GPU if ONNX Runtime is compiled with CUDA, making it portable across 4090/A4000/GH200-class hardware.
- **API sketch:**
  ```rust
  use fastembed::{TextEmbedding, InitOptions, EmbeddingModel};

  // Load model once at startup
  let mut model = TextEmbedding::try_new(
      InitOptions::new(EmbeddingModel::BgeSmallEnV15)
          .with_show_download_progress(true),
  )?;

  // Embed a batch of strings (prefixing "query:" / "passage:" recommended)
  let docs = vec!["query: summarize...", "passage: ..."];
  let embeddings = model.embed(docs, None)?; // returns Vec<Vec<f32>>
  ```
- **Integration path:** add `fastembed = "5"` to `Cargo.toml`, initialize the model in `AppState`, and call `model.embed` for each latest user message. Pair with a simple dot-product similarity search over canonical vectors.

This library removes the need for a separate embedding service, keeps latency low, and supports arbitrary hardware configurations via ONNX Runtime.

---

## 9. Summary

Adding embedding-based routing gives EduRouter a powerful lever for intent-sensitive model selection. By carefully integrating the embedding workflow into the existing planner—respecting cache, stickiness, and policy—we can route “hard” vs. “easy” prompts to the right models while keeping latency low. The outlined plan covers canonical dataset creation, embedding provider choices, code changes, observability, and testing to make the feature production-ready.
