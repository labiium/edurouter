#!/usr/bin/env python3
"""Simple end-to-end check that embedding-aware routing is active."""

from __future__ import annotations

import argparse
import sys
import uuid
from typing import Optional

import requests


def run_check(router_url: str, alias: str, summary: str, expected_model: str) -> None:
    payload = {
        "schema_version": "1.1",
        "request_id": f"embed-check-{uuid.uuid4().hex[:12]}",
        "alias": alias,
        "api": "responses",
        "privacy_mode": "features_only",
        "caps": [],
        "stream": False,
        "conversation": {"summary": summary},
    }
    resp = requests.post(f"{router_url.rstrip('/')}/route/plan", json=payload, timeout=15)
    if resp.status_code != 200:
        raise SystemExit(
            f"/route/plan failed: {resp.status_code} {resp.text.strip()}"
        )
    canonical_header = resp.headers.get("X-Canonical-Model")
    data = resp.json()
    canonical_body: Optional[str] = data.get("canonical", {}).get("model")
    resolved = data.get("upstream", {}).get("model_id")

    if canonical_header != expected_model:
        raise SystemExit(
            f"Expected canonical header {expected_model}, got {canonical_header}"
        )
    if canonical_body != expected_model:
        raise SystemExit(
            f"Expected canonical body {expected_model}, got {canonical_body}"
        )
    if resolved != expected_model:
        raise SystemExit(
            f"Expected upstream model {expected_model}, got {resolved}"
        )

    print(
        f"✓ Embedding routing matched '{summary[:32]}...' -> {expected_model} (route {data['route_id']})"
    )


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(description="Validate embedding-aware routing")
    parser.add_argument("--router-url", default="http://localhost:9099", help="Router base URL")
    parser.add_argument("--alias", default="openai-multimodal", help="Alias to request")
    parser.add_argument("--summary", required=True, help="Conversation summary to embed")
    parser.add_argument("--expected-model", required=True, help="Target model ID")
    args = parser.parse_args(argv)

    run_check(args.router_url, args.alias, args.summary, args.expected_model)
    return 0


if __name__ == "__main__":
    sys.exit(main())
