#!/usr/bin/env python3
"""
Run a live characterization test against a Routiium (or OpenAI) `/v1/chat/completions`
endpoint for a set of multimodal models.

Requires:
  - ROUTIIUM_API_KEY or OPENAI_API_KEY in the environment (e.g., via `.env` or export)
  - openai Python SDK (`pip install openai` or `uv pip install openai`)

Usage:
  python python_tests/test_openai_models.py --samples 3
"""

from __future__ import annotations

import argparse
import json
import os
import statistics
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Dict, List, Optional

from openai import OpenAI, OpenAIError

API_KEY_ENV = "OPENAI_API_KEY"
ROUTIIUM_KEY_ENV = "ROUTIIUM_API_KEY"
BASE_URL_ENV_CANDIDATES = (
    "ROUTIIUM_BASE_URL",
    "OPENAI_BASE_URL",
)
DEFAULT_BASE_URL = "https://api.openai.com/v1"


SAMPLE_IMAGE_URL = (
    "https://upload.wikimedia.org/wikipedia/commons/3/3c/Shaki_waterfall.jpg"
)

PROMPT_TEXT = (
    "You are a helpful assistant. Inspect the image (simple gradient square) and explain which corner "
    "appears brighter. Then summarize in one sentence why this test verifies multimodal capability."
)


@dataclass
class Price:
    input_usd_per_million: float
    cache_input_usd_per_million: float
    output_usd_per_million: float


MODEL_MATRIX: Dict[str, Price] = {
    "gpt-4.1-nano": Price(
        input_usd_per_million=0.20,
        cache_input_usd_per_million=0.05,
        output_usd_per_million=0.80,
    ),
    "gpt-5-nano": Price(
        input_usd_per_million=0.050,
        cache_input_usd_per_million=0.005,
        output_usd_per_million=0.400,
    ),
    "gpt-5-mini": Price(
        input_usd_per_million=0.250,
        cache_input_usd_per_million=0.025,
        output_usd_per_million=2.000,
    ),
}


def load_env_from_file(path: str = ".env") -> None:
    env_path = Path(path)
    if not env_path.exists():
        return
    for raw_line in env_path.read_text().splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        key = key.strip()
        value = value.strip().strip('"').strip("'")
        os.environ.setdefault(key, value)


def require_api_key() -> str:
    for env_name in (ROUTIIUM_KEY_ENV, API_KEY_ENV):
        key = os.getenv(env_name)
        if key:
            return key
    raise RuntimeError(
        "Neither ROUTIIUM_API_KEY nor OPENAI_API_KEY is set. Export one or add it to your .env."
    )


def resolve_base_url() -> str:
    for env_name in BASE_URL_ENV_CANDIDATES:
        raw = os.getenv(env_name)
        if not raw:
            continue
        base = raw.rstrip("/")
        if base.endswith("/chat/completions") or base.endswith("/responses"):
            base = base.rsplit("/", 1)[0]
        if not base.endswith("/v1"):
            base = f"{base}/v1"
        return base
    return DEFAULT_BASE_URL


def create_client(api_key: str) -> OpenAI:
    base_url = resolve_base_url()
    return OpenAI(api_key=api_key, base_url=base_url)


def build_messages() -> List[dict]:
    return [
        {
            "role": "user",
            "content": [
                {"type": "text", "text": PROMPT_TEXT},
                {
                    "type": "image_url",
                    "image_url": {
                        "url": SAMPLE_IMAGE_URL,
                        "detail": "auto",
                    },
                },
            ],
        }
    ]


def usd_from_mtokens(tokens: int, price: float) -> float:
    return (tokens / 1_000_000.0) * price


def estimate_cost(usage: dict, price: Price) -> float:
    input_tokens = usage.get("input_tokens", 0)
    cached_read = usage.get("cache_read_input_tokens", 0)
    cached_create = usage.get("cache_creation_input_tokens", 0)
    output_tokens = usage.get("output_tokens", 0)

    standard_input = max(input_tokens - cached_read - cached_create, 0)

    cost = 0.0
    cost += usd_from_mtokens(standard_input, price.input_usd_per_million)
    cost += usd_from_mtokens(
        cached_read + cached_create, price.cache_input_usd_per_million
    )
    cost += usd_from_mtokens(output_tokens, price.output_usd_per_million)
    return cost


@dataclass
class RunResult:
    model: str
    latency_ms: float
    text: str
    usage: dict
    cost_usd: float


def collect_output_text(data: dict) -> str:
    chunks: List[str] = []
    for entry in data.get("output", []):
        entry_type = entry.get("type")
        if entry_type == "message":
            for part in entry.get("content", []):
                if part.get("type") in ("output_text", "text"):
                    chunks.append(part.get("text", ""))
        elif entry_type in ("output_text", "text"):
            chunks.append(entry.get("text", ""))

    if not chunks and "choices" in data:
        chunks.extend(collect_choice_text(data.get("choices", [])))

    return "\n".join(chunk for chunk in chunks if chunk).strip()


def normalize_usage(usage: Optional[dict]) -> dict:
    if not usage:
        return {}
    if hasattr(usage, "model_dump"):
        usage = usage.model_dump()
    normalized = dict(usage)
    if "input_tokens" not in normalized and "prompt_tokens" in normalized:
        normalized["input_tokens"] = normalized.get("prompt_tokens", 0)
    if "output_tokens" not in normalized and "completion_tokens" in normalized:
        normalized["output_tokens"] = normalized.get("completion_tokens", 0)
    return normalized


def collect_choice_text(choices: List[dict]) -> List[str]:
    chunks: List[str] = []
    for choice in choices:
        message = choice.get("message") or {}
        content = message.get("content")
        if isinstance(content, list):
            for part in content:
                if isinstance(part, dict):
                    text = part.get("text")
                    if text:
                        chunks.append(text)
                elif isinstance(part, str):
                    chunks.append(part)
        elif isinstance(content, str):
            chunks.append(content)
    return chunks


def invoke_model(model: str, client: OpenAI) -> RunResult:
    messages = build_messages()
    started = time.perf_counter()
    resp = client.chat.completions.create(
        model=model,
        messages=messages,
        max_completion_tokens=256,
    )
    latency = (time.perf_counter() - started) * 1000.0
    data = resp.model_dump()

    text = collect_output_text(data)

    usage = normalize_usage(data.get("usage"))
    cost = estimate_cost(usage, MODEL_MATRIX[model])

    return RunResult(
        model=model,
        latency_ms=latency,
        text=text,
        usage=usage,
        cost_usd=cost,
    )


def summarize(results: List[RunResult]) -> dict:
    latencies = [r.latency_ms for r in results]
    costs = [r.cost_usd for r in results]
    return {
        "latency_ms": {
            "min": min(latencies),
            "avg": statistics.fmean(latencies),
            "max": max(latencies),
        },
        "cost_usd": {
            "min": min(costs),
            "avg": statistics.fmean(costs),
            "max": max(costs),
        },
    }


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Live test for OpenAI multimodal models."
    )
    parser.add_argument(
        "--samples",
        type=int,
        default=1,
        help="Number of iterations per model (default: 1).",
    )
    parser.add_argument(
        "--output",
        default="openai_model_characterization.json",
        help="Path to write the JSON report.",
    )
    args = parser.parse_args()

    load_env_from_file()
    api_key = require_api_key()
    client = create_client(api_key)

    all_results: List[RunResult] = []
    for model in MODEL_MATRIX:
        for i in range(args.samples):
            print(f"Invoking {model} [sample {i + 1}/{args.samples}]...")
            try:
                result = invoke_model(model, client)
                all_results.append(result)
                print(
                    f" - latency: {result.latency_ms:.2f} ms | "
                    f"cost: ${result.cost_usd:.6f} | tokens: {result.usage}"
                )
            except OpenAIError as exc:
                print(f"[ERROR] {model} failed: {exc}", file=sys.stderr)
                raise

    summary = summarize(all_results)
    report = {
        "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "samples_per_model": args.samples,
        "results": [r.__dict__ for r in all_results],
        "summary": summary,
    }

    with open(args.output, "w", encoding="utf-8") as fh:
        json.dump(report, fh, indent=2)
    print(f"\nWrote characterization report to {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
