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
ROUTIIUM_URL = os.getenv("ROUTIIUM_URL", "http://routiium:8080")
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
    cache_state: str
    route_id: str
    tier: Optional[str]


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
        "alias": "edu-general",
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

    resp.raise_for_status()
    validate_headers(resp.headers)
    body = resp.json()

    return PlanResult(
        status=resp.status_code,
        latency_ms=latency,
        cache_state=resp.headers.get("X-Route-Cache", "unknown"),
        route_id=body["route_id"],
        tier=resp.headers.get("X-Route-Tier"),
    )


def exercise_router(samples: int, concurrency: int) -> List[PlanResult]:
    results: List[PlanResult] = []
    with ThreadPoolExecutor(max_workers=concurrency) as executor:
        futures = [executor.submit(plan_once, i) for i in range(samples)]
        for future in as_completed(futures):
            results.append(future.result())
    return results


def summarize(results: List[PlanResult]) -> dict:
    latencies = [r.latency_ms for r in results]
    cache_states = {}
    for r in results:
        cache_states[r.cache_state] = cache_states.get(r.cache_state, 0) + 1
    return {
        "samples": len(results),
        "latency_ms": {
            "min": min(latencies),
            "avg": statistics.fmean(latencies),
            "p95": statistics.quantiles(latencies, n=100)[94],
            "max": max(latencies),
        },
        "cache_states": cache_states,
    }


def main() -> int:
    print("Waiting for edurouter...")
    wait_for(f"{ROUTER_URL}/healthz")

    print("Attempting to reach routiium image (optional)...")
    try:
        wait_for(ROUTIIUM_URL, timeout=5)
    except RuntimeError:
        print("Warning: routiium container did not expose an HTTP endpoint", file=sys.stderr)

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
    return 0


if __name__ == "__main__":
    sys.exit(main())
