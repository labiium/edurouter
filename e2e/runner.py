import json
import os
import statistics
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass
from typing import List, Optional

import requests

ROUTER_URL = os.getenv("ROUTER_URL", "http://edurouter:9099")
ROUTIIUM_URL = os.getenv("ROUTIIUM_URL", "http://routiium:8088")
SAMPLE_REQUESTS = int(os.getenv("SAMPLE_REQUESTS", "50"))
CONCURRENCY = int(os.getenv("CONCURRENCY", "4"))
OUTPUT_PATH = os.getenv("OUTPUT_PATH", "./perf_report.json")
TIMEOUT = float(os.getenv("REQUEST_TIMEOUT", "15"))

MANDATORY_HEADERS = [
    "Router-Schema",
    "Router-Latency",
    "Config-Revision",
    "Catalog-Revision",
    "X-Route-Cache",
    "X-Route-Id",
    "X-Resolved-Model",
    "X-Route-Tier",
    "X-Policy-Rev",
    "X-Content-Used",
]


@dataclass
class PlanResult:
    status: int
    latency_ms: float
    cache_state: Optional[str] = None
    route_id: Optional[str] = None
    tier: Optional[str] = None
    error_code: Optional[str] = None
    error_message: Optional[str] = None


def wait_for(url: str, timeout: float = 60.0) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            response = requests.get(url, timeout=3)
            if response.ok:
                return
        except requests.RequestException:
            pass
        time.sleep(1)
    raise RuntimeError(f"Timed out waiting for {url}")


def validate_headers(headers: requests.structures.CaseInsensitiveDict) -> None:
    missing = [header for header in MANDATORY_HEADERS if header not in headers]
    if missing:
        raise AssertionError(f"Missing headers from router response: {missing}")


def plan_once(idx: int) -> PlanResult:
    payload = {
        "schema_version": "1.1",
        "request_id": f"e2e-{idx}",
        "alias": "openai-multimodal",
        "api": "responses",
        "privacy_mode": "features_only",
        "stream": False,
        "caps": ["text"],
    }

    started = time.perf_counter()
    resp = requests.post(
        f"{ROUTER_URL}/route/plan",
        json=payload,
        timeout=TIMEOUT,
    )
    latency = (time.perf_counter() - started) * 1000.0

    body: Optional[dict] = None
    try:
        body = resp.json()
    except ValueError:
        body = None

    if resp.ok:
        validate_headers(resp.headers)
        return PlanResult(
            status=resp.status_code,
            latency_ms=latency,
            cache_state=resp.headers.get("X-Route-Cache", "unknown"),
            route_id=body["route_id"],
            tier=resp.headers.get("X-Route-Tier"),
        )

    return PlanResult(
        status=resp.status_code,
        latency_ms=latency,
        error_code=(body or {}).get("code"),
        error_message=(body or {}).get("message") or resp.text,
    )


def exercise_router(samples: int, concurrency: int) -> List[PlanResult]:
    results: List[PlanResult] = []
    with ThreadPoolExecutor(max_workers=concurrency) as executor:
        futures = [executor.submit(plan_once, i) for i in range(samples)]
        for future in as_completed(futures):
            results.append(future.result())
    return results


def summarize(results: List[PlanResult]) -> dict:
    successes = [r for r in results if 200 <= r.status < 300]
    errors = [r for r in results if r.status >= 400]

    latency_block = None
    cache_states = {}
    if successes:
        latencies = [r.latency_ms for r in successes]
        latency_block = {
            "min": min(latencies),
            "avg": statistics.fmean(latencies),
            "p95": statistics.quantiles(latencies, n=100)[94],
            "max": max(latencies),
        }
        for r in successes:
            cache_states[r.cache_state or "unknown"] = (
                cache_states.get(r.cache_state or "unknown", 0) + 1
            )

    error_counts = {}
    for r in errors:
        key = r.error_code or f"HTTP_{r.status}"
        error_counts[key] = error_counts.get(key, 0) + 1

    return {
        "samples": len(results),
        "successes": len(successes),
        "errors": len(errors),
        "latency_ms": latency_block,
        "cache_states": cache_states,
        "error_breakdown": error_counts,
        "sample_error": errors[0].__dict__ if errors else None,
    }


def main() -> int:
    print("Waiting for edurouter...")
    wait_for(f"{ROUTER_URL}/healthz")

    print("Attempting to reach routiium image (optional)...")
    try:
        wait_for(ROUTIIUM_URL, timeout=5)
    except RuntimeError:
        print(
            "Warning: routiium container did not expose an HTTP endpoint",
            file=sys.stderr,
        )

    print(f"Sending {SAMPLE_REQUESTS} plan requests with concurrency={CONCURRENCY}")
    results = exercise_router(SAMPLE_REQUESTS, CONCURRENCY)
    report = summarize(results)
    print(json.dumps(report, indent=2))

    with open(OUTPUT_PATH, "w", encoding="utf-8") as fh:
        json.dump(
            {
                "router_url": ROUTER_URL,
                "routiium_url": ROUTIIUM_URL,
                "samples": SAMPLE_REQUESTS,
                "concurrency": CONCURRENCY,
                "report": report,
            },
            fh,
            indent=2,
        )
    print(f"Wrote report to {OUTPUT_PATH}")

    if report["errors"] > 0:
        print(
            "Some plan requests failed. See sample_error section above.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
