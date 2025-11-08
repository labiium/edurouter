# EduRouter Roadmap

This roadmap distills the spec checklist into sequenced implementation phases so EduRouter is ready for the simulator and paper. Earlier phases unblock later ones; do them in order whenever possible.

## Phase 0 – Contract Reset
- Replace the error envelope everywhere with the typed body (`schema_version`, `code`, `message`, `request_id`, `policy_rev`, `retry_hint_ms`) and align HTTP status mappings with the spec table.
- Update README/API docs and any client examples to reflect the typed error structure so Routiium and tests are consistent.
- Add a shared helper/util to emit errors so every endpoint (including `/route/plan`) stays in sync.

## Phase 1 – /route/plan Surface
- Emit the full header set on every successful response: `Router-Schema: 1.1`, `Router-Latency`, `Config-Revision`, `Catalog-Revision`, `X-Route-Cache`, `X-Route-Id`, `X-Resolved-Model`, `X-Route-Tier`, `X-Policy-Rev`, `X-Content-Used`, optional `X-Route-Why` and `X-Route-Provider`.
- Ensure the RoutePlan JSON always includes the required fields (upstream, limits, prompt overlays with fingerprint/size, hints w/ costs and latency, fallbacks, cache block, stickiness block, policy + `policy_rev`, `content_used`, `governance_echo`).
- Parse `privacy_mode` + `content_attestation.included`, compute `content_used`, and echo it both in the body and headers. Accept and propagate `traceparent`/`tracestate`, and make sure the `request_id` is echoed everywhere.

## Phase 2 – Cache & Stickiness Semantics
- Rebuild the cache key to include alias, `policy_rev`, overlay fingerprint or ID, privacy mode, capability caps, and API mode.
- Respect `ttl_ms` vs `valid_until`, honor `freeze_key`, and emit `X-Route-Cache` states (miss/hit/stale) correctly.
- Implement stickiness intake: accept incoming `plan_token`, enforce `max_turns`/`expires_at`, rotate the token when recomputing, and sign tokens with `ROUTER_STICKY_SECRET`, warning if the default secret is in use.

## Phase 3 – Catalog & Scoring
- Have `GET /catalog/models` return the catalog payload with both strong/weak ETags and the catalog revision in headers.
- Ensure each model entry exposes cost (micro USD), capabilities, limits, recent SLOs, and health status; update scoring to skip or penalize unhealthy entries and to surface fallbacks when primaries degrade.
- Include catalog revision + policy revision in both headers and plan body so downstream sims can bucket results.

## Phase 4 – Config Pack for Sims
- Ship `configs/policy.json` describing T1/T3 tiers, escalation predicates (token length, uncertainty regex, scpi errors), approvals, budgets, and overlay mappings.
- Ship `configs/catalog.json` with at least one healthy T1 and one T3 model including prices, capabilities, SLO latencies, and statuses.
- Add overlay files under `configs/overlays/` (e.g., `troubleshoot-first.txt`, `assessment-mode.txt`) and store their SHA-256 fingerprints for the `prompt_overlays` block.

## Phase 5 – Simulation-Facing Behavior & Guardrails
- Enforce overlay size limits and reject violations with `POLICY_DENY`.
- Ensure every plan includes `hints.est_cost_micro`, `hints.est_latency_ms`, `policy.revision`, and `prompt_overlays.overlay_fingerprint` for simulator analytics.
- Add request body size guards that return `INVALID_REQUEST` on overflow and a simple per-IP token-bucket rate limiter for `/route/plan` to prevent accidental DoS during sims.

## Phase 6 – Contract Tests & Diagnostics
- Create a `tests/contract.rs` (or similar) suite with goldens that cover schema/headers, cache/stickiness transitions, escalation + `teacher_boost`, privacy behaviors, catalog fallback selection, `/catalog/models` ETag behavior, and the main error modes (`ALIAS_UNKNOWN`, `UNSUPPORTED_SCHEMA`, optional `BUDGET_EXCEEDED`).
- Block merges on this suite so regressions against the spec are caught immediately.

## Phase 7 – Ops & Observability
- Add structured logs carrying `request_id`, trace context, cache hits, stickiness events, and policy/catalog revisions.
- Keep admin reload endpoints from the README implemented and ACL-protected so configs can refresh without a restart.
- Document operational steps (reload order, required env vars like `ROUTER_STICKY_SECRET`) to make simulator bring-up repeatable.

Following these phases delivers the spec-critical requirements first (errors, headers, RoutePlan schema, cache/stickiness), then the config artifacts, tests, and ops polish needed for deterministic simulator runs and the paper.
