#!/usr/bin/env python3
"""
Simple terminal chat UI that routes through EduRouter + Routiium to OpenAI.

Requirements
------------
1. EduRouter running locally (or reachable via ROUTER_URL) with aliases that point to OpenAI.
2. Routiium pointed at EduRouter (optional, but recommended for tracing).
3. OPENAI_API_KEY present in your environment or .env file.

Usage
-----
python python_tests/chat_router_demo.py

Environment variables
---------------------
ROUTER_URL         (default: http://localhost:9099)
ROUTER_ALIAS       (default: openai-multimodal)
ROUTER_PRIVACY     (default: features_only)
ROUTER_CAPS        (default: text,image)
"""

from __future__ import annotations

import json
import os
import signal
import sys
import time
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Dict, List, Optional

import requests
from openai import OpenAI, OpenAIError

DEFAULT_ALIAS = "openai-multimodal"
DEFAULT_ROUTER_URL = "http://localhost:9099"
DEFAULT_ROUTIIUM_URL = "http://localhost:8088"
DEFAULT_ROUTIIUM_LABEL = "chat-demo"
DEFAULT_ROUTIIUM_TTL = 86400  # 1 day


def load_env() -> None:
    path = Path(".env")
    if not path.exists():
        return
    for raw_line in path.read_text().splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        key = key.strip()
        value = value.strip().strip('"').strip("'")
        os.environ.setdefault(key, value)


def get_env_array(name: str, default: str) -> List[str]:
    raw = os.getenv(name, default)
    return [part.strip() for part in raw.split(",") if part.strip()]


def maybe_generate_routiium_key(base_url: str) -> Optional[str]:
    """
    Attempt to generate a Routiium client access token via /keys/generate.
    Intended for local dev only. Requires the endpoint to be exposed without auth.
    """
    auto_toggle = os.getenv("ROUTIIUM_AUTO_KEY", "1").lower()
    if auto_toggle in {"0", "false", "off"}:
        return None
    label = os.getenv("ROUTIIUM_KEY_LABEL", DEFAULT_ROUTIIUM_LABEL)
    ttl = int(os.getenv("ROUTIIUM_KEY_TTL", DEFAULT_ROUTIIUM_TTL))
    url = f"{base_url.rstrip('/')}/keys/generate"
    try:
        resp = requests.post(
            url,
            json={"label": label, "ttl_seconds": ttl},
            timeout=10,
        )
        resp.raise_for_status()
        token = resp.json().get("token")
        if token:
            os.environ["ROUTIIUM_API_KEY"] = token
            os.environ.setdefault("OPENAI_API_KEY", token)
            cache_path = Path(".routiium_token")
            cache_path.write_text(token, encoding="utf-8")
            print(
                f"[info] Generated Routiium key '{label}'. Cached in {cache_path}. "
                "Set ROUTIIUM_API_KEY to reuse it later."
            )
            return token
    except Exception as exc:
        print(f"[warn] Could not auto-generate Routiium key: {exc}")
    return None


def get_cached_routiium_token() -> Optional[str]:
    cache_path = Path(".routiium_token")
    if not cache_path.exists():
        return None
    token = cache_path.read_text(encoding="utf-8").strip()
    if token:
        os.environ.setdefault("ROUTIIUM_API_KEY", token)
        os.environ.setdefault("OPENAI_API_KEY", token)
        return token
    return None


def get_or_create_routiium_token(base_url: str) -> Optional[str]:
    token = os.getenv("ROUTIIUM_API_KEY")
    if token:
        return token
    token = get_cached_routiium_token()
    if token:
        return token
    token = os.getenv("OPENAI_API_KEY")
    if token:
        return token
    return maybe_generate_routiium_key(base_url)


@dataclass
class PlanMeta:
    plan: Dict
    headers: Dict


def fetch_plan(
    router_url: str,
    alias: str,
    privacy_mode: str,
    caps: List[str],
    conversation: List[Dict[str, str]],
    user_message: str,
) -> PlanMeta:
    history_preview = "\n".join(
        [f"{turn['role'].upper()}: {turn['content']}" for turn in conversation[-4:]]
    )
    payload = {
        "schema_version": "1.1",
        "request_id": str(uuid.uuid4()),
        "alias": alias,
        "api": "responses",
        "privacy_mode": privacy_mode,
        "stream": False,
        "caps": caps,
        "conversation": {
            "summary": history_preview,
            "turns": len(conversation),
        },
        "overrides": {},
        "estimates": {
            "prompt_tokens": 2048,
            "max_output_tokens": 512,
        },
    }
    resp = requests.post(
        f"{router_url.rstrip('/')}/route/plan", json=payload, timeout=30
    )
    resp.raise_for_status()
    return PlanMeta(plan=resp.json(), headers=dict(resp.headers))


def ensure_api_base(url: str) -> str:
    base = url.rstrip("/")
    if base.endswith("/responses") or base.endswith("/chat/completions"):
        base = base.rsplit("/", 1)[0]
    if not base.endswith("/v1"):
        base = f"{base}/v1"
    return base


def build_messages(
    plan: Dict, conversation: List[Dict[str, str]], user_message: str
) -> List[Dict[str, str]]:
    overlay = plan.get("prompt_overlays", {}).get("system_overlay")
    messages: List[Dict[str, str]] = []
    if overlay:
        messages.append({"role": "system", "content": overlay.strip()})
    for turn in conversation:
        role = turn.get("role", "user")
        if role not in {"assistant", "user", "system"}:
            role = "user"
        messages.append({"role": role, "content": turn.get("content", "")})
    messages.append({"role": "user", "content": user_message})
    return messages


def resolve_api_key(upstream: Dict) -> str:
    auth_env = upstream.get("auth_env") or "OPENAI_API_KEY"
    api_key = os.getenv(auth_env)
    if api_key:
        return api_key
    routiium_base = os.getenv("ROUTIIUM_URL", DEFAULT_ROUTIIUM_URL).rstrip("/")
    token = get_or_create_routiium_token(routiium_base)
    if token:
        os.environ.setdefault(auth_env, token)
        return token
    raise RuntimeError(
        f"{auth_env} is not set. Export it or allow ROUTIIUM_AUTO_KEY=1 to auto-generate."
    )


def create_chat_client(upstream: Dict) -> OpenAI:
    base_url = ensure_api_base(upstream["base_url"])
    api_key = resolve_api_key(upstream)
    headers = upstream.get("headers") or {}
    return OpenAI(
        api_key=api_key,
        base_url=base_url,
        default_headers=headers if headers else None,
    )


def messages_to_response_input(messages: List[Dict[str, str]]) -> List[Dict[str, Any]]:
    formatted: List[Dict[str, Any]] = []
    for message in messages:
        text = message.get("content", "")
        if not isinstance(text, str):
            text = str(text)
        formatted.append(
            {
                "role": message.get("role", "user"),
                "content": [
                    {
                        "type": "input_text",
                        "text": text,
                    }
                ],
            }
        )
    return formatted


def invoke_llm(plan: Dict, messages: List[Dict[str, str]]) -> Dict:
    upstream = plan["upstream"]
    client = create_chat_client(upstream)
    max_tokens = plan.get("limits", {}).get("max_output_tokens") or 512
    route_id = plan.get("route_id")
    metadata = {
        "route_id": route_id,
        "policy_revision": plan.get("policy_rev"),
    }
    metadata = {k: v for k, v in metadata.items() if v}
    mode = (upstream.get("mode") or "responses").lower()

    if "chat" in mode:
        create_kwargs: Dict[str, Any] = {
            "model": upstream["model_id"],
            "messages": messages,
            "max_completion_tokens": max_tokens,
        }
        if route_id:
            create_kwargs["user"] = route_id[:64]
        response = client.chat.completions.create(**create_kwargs)
    else:
        create_kwargs = {
            "model": upstream["model_id"],
            "input": messages_to_response_input(messages),
            "max_output_tokens": max_tokens,
        }
        if metadata:
            create_kwargs["metadata"] = metadata
        response = client.responses.create(**create_kwargs)
    return response.model_dump()


def extract_text(output: Dict) -> str:
    def gather_text(node: Any) -> List[str]:
        collected: List[str] = []
        if isinstance(node, dict):
            entry_type = node.get("type")
            if entry_type in {"output_text", "text"}:
                value = node.get("text")
                if isinstance(value, str):
                    collected.append(value)
                elif isinstance(value, list):
                    for item in value:
                        if isinstance(item, str):
                            collected.append(item)
                        elif isinstance(item, dict):
                            text_val = item.get("text")
                            if text_val:
                                collected.append(text_val)
            content = node.get("content")
            if isinstance(content, list):
                for child in content:
                    collected.extend(gather_text(child))
        elif isinstance(node, list):
            for child in node:
                collected.extend(gather_text(child))
        return collected

    chunks: List[str] = []
    for entry in output.get("output", []):
        chunks.extend(gather_text(entry))
    text = "\n".join(
        chunk.strip() for chunk in chunks if isinstance(chunk, str) and chunk.strip()
    )
    if not text and "choices" in output:
        for choice in output.get("choices", []):
            message = choice.get("message") or {}
            content_txt = message.get("content")
            if isinstance(content_txt, str):
                text += ("\n" if text else "") + content_txt.strip()
            elif isinstance(content_txt, list):
                for seg in content_txt:
                    if isinstance(seg, str):
                        text += ("\n" if text else "") + seg.strip()
                    elif isinstance(seg, dict):
                        value = seg.get("text")
                        if value:
                            text += ("\n" if text else "") + value.strip()

    if not text:
        Path("chat_router_demo_last_response.json").write_text(
            json.dumps(output, indent=2), encoding="utf-8"
        )
        fallback_bits = []
        status = output.get("status")
        if status:
            fallback_bits.append(f"status={status}")
        incomplete = output.get("incomplete_details") or {}
        reason = incomplete.get("reason")
        if reason:
            fallback_bits.append(f"reason={reason}")
        usage = output.get("usage") or {}
        out_tokens = usage.get("output_tokens")
        if out_tokens:
            fallback_bits.append(f"output_tokens={out_tokens}")
        details = (
            ", ".join(fallback_bits) if fallback_bits else "model returned no text"
        )
        return f"[no text returned: {details}]"
    return text


def human_readable_cost(plan: Dict) -> str:
    hints = plan.get("hints") or {}
    est_cost_micro = hints.get("est_cost_micro")
    if not est_cost_micro:
        return "n/a"
    return f"${est_cost_micro / 1_000_000:.6f}"


def print_router_snapshot(meta: PlanMeta) -> None:
    plan = meta.plan
    headers = meta.headers
    hints = plan.get("hints") or {}
    latency = headers.get("Router-Latency")
    if not latency:
        latency = f"{headers.get('X-Route-Latency-ms', 'n/a')} ms"
    cache_state = headers.get("X-Route-Cache") or "n/a"
    print("\n--- Router Snapshot --------------------------------")
    print(f"Route ID    : {plan['route_id']}")
    print(f"Tier / Model: {hints.get('tier')} / {plan['upstream']['model_id']}")
    print(f"Cache State : {cache_state}")
    print(f"Latency     : {latency}")
    print(f"Plan Cost   : {human_readable_cost(plan)}")
    print("----------------------------------------------------\n")


def graceful_exit(*_) -> None:
    print("\nExiting chat. Bye!")
    sys.exit(0)


def politely_serialize_headers(headers: Dict[str, str], plan: Dict) -> Dict:
    return {
        "route_id": plan.get("route_id"),
        "cache": headers.get("X-Route-Cache"),
        "latency_ms": headers.get("Router-Latency"),
        "model": plan.get("upstream", {}).get("model_id"),
        "tier": plan.get("hints", {}).get("tier"),
    }


def main() -> int:
    load_env()
    signal.signal(signal.SIGINT, graceful_exit)
    signal.signal(signal.SIGTERM, graceful_exit)

    router_url = os.getenv("ROUTER_URL", DEFAULT_ROUTER_URL)
    alias = os.getenv("ROUTER_ALIAS", DEFAULT_ALIAS)
    privacy = os.getenv("ROUTER_PRIVACY", "features_only")
    caps = get_env_array("ROUTER_CAPS", "text,image")

    print("EduRouter Chat Demo")
    print("-------------------")
    print(f"Router URL   : {router_url}")
    print(f"Alias        : {alias}")
    print(f"Capabilities : {', '.join(caps)}")
    print("Press Ctrl+C to exit.\n")

    conversation: List[Dict[str, str]] = []

    while True:
        user_message = input("You: ").strip()
        if not user_message:
            continue
        conversation.append({"role": "user", "content": user_message})

        try:
            plan_meta = fetch_plan(
                router_url, alias, privacy, caps, conversation, user_message
            )
        except requests.HTTPError as exc:
            print(f"[router error] {exc} -> {exc.response.text}")
            continue

        print_router_snapshot(plan_meta)
        messages = build_messages(plan_meta.plan, conversation[:-1], user_message)

        try:
            start = time.perf_counter()
            llm_resp = invoke_llm(plan_meta.plan, messages)
            latency = (time.perf_counter() - start) * 1000.0
            assistant_text = extract_text(llm_resp)
        except OpenAIError as exc:
            print(f"[LLM error] {exc}")
            continue
        except RuntimeError as exc:
            print(f"[config error] {exc}")
            continue

        conversation.append({"role": "assistant", "content": assistant_text})
        print(f"Assistant ({latency:.1f} ms):\n{assistant_text}\n")
        print(
            "[trace]",
            json.dumps(politely_serialize_headers(plan_meta.headers, plan_meta.plan)),
        )


if __name__ == "__main__":
    raise SystemExit(main())
