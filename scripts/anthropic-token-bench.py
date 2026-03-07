#!/usr/bin/env python3
"""Anthropic token counting benchmark for writ TOON format.

Sends sample writ output (JSON vs TOON) to the Anthropic API's token counting
endpoint to get ground-truth Claude token counts. This gives exact numbers
for Claude models, complementing the tiktoken-rs (cl100k_base) benchmarks
in the Rust test suite.

Usage:
    export ANTHROPIC_API_KEY=sk-ant-...
    python scripts/anthropic-token-bench.py

    # With a live writ repo:
    python scripts/anthropic-token-bench.py --live

Requirements:
    pip install anthropic

This script is NOT part of the writ package. It's a dev/benchmarking tool only.
"""

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

try:
    import anthropic
except ImportError:
    print("error: anthropic package not installed. Run: pip install anthropic")
    sys.exit(1)


def count_tokens(client: anthropic.Anthropic, text: str, model: str = "claude-sonnet-4-20250514") -> int:
    """Count tokens using the Anthropic API's token counting endpoint."""
    response = client.messages.count_tokens(
        model=model,
        messages=[{"role": "user", "content": text}],
    )
    return response.input_tokens


def savings_pct(baseline: int, candidate: int) -> float:
    if baseline == 0:
        return 0.0
    return ((baseline - candidate) / baseline) * 100.0


def generate_sample_context_json() -> str:
    """Generate a realistic writ context payload in JSON format."""
    context = {
        "writ_version": "0.1.0",
        "active_spec": {
            "id": "auth",
            "title": "Authentication system",
            "description": "JWT-based auth with refresh tokens and RBAC",
            "status": "in-progress",
        },
        "all_specs": [
            {
                "id": "auth",
                "title": "Authentication system",
                "description": "JWT-based auth with refresh tokens and RBAC",
                "status": "in-progress",
            },
            {
                "id": "convergence",
                "title": "Convergence v2",
                "description": "Three-way merge with conflict resolution pipeline",
                "status": "complete",
            },
            {
                "id": "format",
                "title": "Output format system",
                "description": "TOON format for token-efficient LLM output",
                "status": "in-progress",
            },
            {
                "id": "gc",
                "title": "Garbage collection",
                "description": "Lifecycle state machine and storage cleanup",
                "status": "complete",
            },
            {
                "id": "security",
                "title": "Security sprint",
                "description": "BLAKE3 hashing, Ed25519 signing, scope enforcement",
                "status": "complete",
            },
        ],
        "working_state": {
            "clean": False,
            "new_files": ["src/auth/jwt.rs", "src/auth/rbac.rs", "src/auth/mod.rs"],
            "modified_files": [
                "src/repo.rs",
                "src/config.rs",
                "src/hooks.rs",
                "src/format.rs",
                "src/main.rs",
            ],
            "deleted_files": ["src/old_auth.rs"],
            "tracked_count": 47,
        },
        "recent_seals": [
            {
                "id": "a1b2c3d4e5f6",
                "timestamp": "2026-03-06T14:30:00Z",
                "agent": "cc-opus",
                "summary": "Implemented JWT token generation and validation",
                "files_changed": 3,
                "spec_id": "auth",
                "status": "in-progress",
            },
            {
                "id": "b2c3d4e5f6a7",
                "timestamp": "2026-03-06T14:15:00Z",
                "agent": "amis-sonnet",
                "summary": "Added RBAC middleware with role hierarchy",
                "files_changed": 2,
                "spec_id": "auth",
                "status": "in-progress",
            },
            {
                "id": "c3d4e5f6a7b8",
                "timestamp": "2026-03-06T13:45:00Z",
                "agent": "bri-haiku",
                "summary": "Added 24 auth integration tests",
                "files_changed": 4,
                "spec_id": "auth",
                "status": "in-progress",
            },
            {
                "id": "d4e5f6a7b8c9",
                "timestamp": "2026-03-06T13:00:00Z",
                "agent": "cc-opus",
                "summary": "Finalized convergence phase 5 with LLM backend",
                "files_changed": 6,
                "spec_id": "convergence",
                "status": "complete",
            },
            {
                "id": "e5f6a7b8c9d0",
                "timestamp": "2026-03-06T12:30:00Z",
                "agent": "haris-sonnet",
                "summary": "Config resolution chain for workflow modes",
                "files_changed": 2,
                "spec_id": "format",
                "status": "in-progress",
            },
        ],
        "integration_risk": {"level": "medium", "score": 35, "factors": ["2 agents on auth spec"]},
        "convergence_recommended": False,
        "session_complete": False,
        "tracked_files": 47,
        "file_scope": [
            f"src/module_{i}/file_{j}.rs" for i in range(8) for j in range(6)
        ],
        "available_operations": ["seal", "context", "log", "converge", "verify", "status", "diff"],
    }
    return json.dumps(context, indent=2)


def generate_sample_context_toon() -> str:
    """Generate the same context in TOON-like format."""
    return """writ_version: 0.1.0

active_spec:
  id: auth
  title: Authentication system
  description: JWT-based auth with refresh tokens and RBAC
  status: in-progress

all_specs:
  - id: auth | title: Authentication system | status: in-progress
  - id: convergence | title: Convergence v2 | status: complete
  - id: format | title: Output format system | status: in-progress
  - id: gc | title: Garbage collection | status: complete
  - id: security | title: Security sprint | status: complete

working_state:
  clean: false
  new_files: src/auth/jwt.rs, src/auth/rbac.rs, src/auth/mod.rs
  modified_files: src/repo.rs, src/config.rs, src/hooks.rs, src/format.rs, src/main.rs
  deleted_files: src/old_auth.rs
  tracked_count: 47

recent_seals:
  - a1b2c3d4e5f6 | 2026-03-06T14:30:00Z | cc-opus | Implemented JWT token generation and validation | 3 files | auth | in-progress
  - b2c3d4e5f6a7 | 2026-03-06T14:15:00Z | amis-sonnet | Added RBAC middleware with role hierarchy | 2 files | auth | in-progress
  - c3d4e5f6a7b8 | 2026-03-06T13:45:00Z | bri-haiku | Added 24 auth integration tests | 4 files | auth | in-progress
  - d4e5f6a7b8c9 | 2026-03-06T13:00:00Z | cc-opus | Finalized convergence phase 5 with LLM backend | 6 files | convergence | complete
  - e5f6a7b8c9d0 | 2026-03-06T12:30:00Z | haris-sonnet | Config resolution chain for workflow modes | 2 files | format | in-progress

integration_risk:
  level: medium
  score: 35
  factors: 2 agents on auth spec

convergence_recommended: false
session_complete: false
tracked_files: 47
file_scope: src/module_0/file_0.rs, src/module_0/file_1.rs, src/module_0/file_2.rs, src/module_0/file_3.rs, src/module_0/file_4.rs, src/module_0/file_5.rs, src/module_1/file_0.rs, src/module_1/file_1.rs, src/module_1/file_2.rs, src/module_1/file_3.rs, src/module_1/file_4.rs, src/module_1/file_5.rs, src/module_2/file_0.rs, src/module_2/file_1.rs, src/module_2/file_2.rs, src/module_2/file_3.rs, src/module_2/file_4.rs, src/module_2/file_5.rs, src/module_3/file_0.rs, src/module_3/file_1.rs, src/module_3/file_2.rs, src/module_3/file_3.rs, src/module_3/file_4.rs, src/module_3/file_5.rs, src/module_4/file_0.rs, src/module_4/file_1.rs, src/module_4/file_2.rs, src/module_4/file_3.rs, src/module_4/file_4.rs, src/module_4/file_5.rs, src/module_5/file_0.rs, src/module_5/file_1.rs, src/module_5/file_2.rs, src/module_5/file_3.rs, src/module_5/file_4.rs, src/module_5/file_5.rs, src/module_6/file_0.rs, src/module_6/file_1.rs, src/module_6/file_2.rs, src/module_6/file_3.rs, src/module_6/file_4.rs, src/module_6/file_5.rs, src/module_7/file_0.rs, src/module_7/file_1.rs, src/module_7/file_2.rs, src/module_7/file_3.rs, src/module_7/file_4.rs, src/module_7/file_5.rs
available_operations: seal, context, log, converge, verify, status, diff"""


def generate_sample_seal_log_json() -> str:
    """Generate a realistic seal log in JSON."""
    seals = []
    agents = ["cc-opus", "amis-sonnet", "bri-haiku", "lee-sonnet", "haris-sonnet"]
    specs = ["auth", "convergence", "format", None, "gc"]
    for i in range(20):
        seal = {
            "id": f"{i*0xDEAD+0xBEEF:012x}",
            "parent": f"{(i-1)*0xDEAD+0xBEEF:012x}" if i > 0 else None,
            "tree_hash": f"tree_{i*31337:08x}",
            "timestamp": f"2026-03-06T{10 + i // 6}:{(i * 10) % 60:02d}:00Z",
            "agent": {"id": agents[i % 5], "agent_type": "agent"},
            "spec_id": specs[i % 5],
            "status": "complete" if i == 19 else "in-progress",
            "summary": f"Implemented feature {i} with {(i % 5) + 1} file changes across module",
            "changes": [
                {
                    "path": f"src/module_{i % 8}/file_{j}.rs",
                    "change_type": "added" if j == 0 else "modified",
                    "old_hash": None if j == 0 else f"old_{i * 100 + j:08x}",
                    "new_hash": f"new_{i * 100 + j:08x}",
                }
                for j in range((i % 5) + 1)
            ],
            "verification": {
                "tests_passed": i * 10 + 100,
                "tests_failed": 0,
                "linted": True,
            },
        }
        seals.append(seal)
    return json.dumps(seals, indent=2)


def generate_sample_seal_log_toon() -> str:
    """Generate the same seal log in TOON-like format."""
    agents = ["cc-opus", "amis-sonnet", "bri-haiku", "lee-sonnet", "haris-sonnet"]
    specs = ["auth", "convergence", "format", "-", "gc"]
    lines = ["seal_log:"]
    for i in range(20):
        status = "complete" if i == 19 else "in-progress"
        lines.append(
            f"  - {i*0xDEAD+0xBEEF:012x} | 2026-03-06T{10 + i // 6}:{(i * 10) % 60:02d}:00Z "
            f"| {agents[i % 5]} | {specs[i % 5]} | {status} "
            f"| Implemented feature {i} with {(i % 5) + 1} file changes across module "
            f"| {(i % 5) + 1} files | tests:{i * 10 + 100}/0"
        )
    return "\n".join(lines)


def run_live_benchmark(client: anthropic.Anthropic, model: str) -> None:
    """Run benchmark against live writ repo output."""
    print("\n--- Live Repo Benchmark ---")

    try:
        json_output = subprocess.run(
            ["writ", "context", "--format", "json"],
            capture_output=True, text=True, check=True,
        ).stdout
        toon_output = subprocess.run(
            ["writ", "context", "--format", "toon"],
            capture_output=True, text=True, check=True,
        ).stdout
    except (subprocess.CalledProcessError, FileNotFoundError) as e:
        print(f"  skipped: {e}")
        return

    json_tokens = count_tokens(client, json_output, model)
    toon_tokens = count_tokens(client, toon_output, model)
    savings = savings_pct(json_tokens, toon_tokens)

    print(f"  JSON context: {json_tokens:>5} tokens ({len(json_output):>6} bytes)")
    print(f"  TOON context: {toon_tokens:>5} tokens ({len(toon_output):>6} bytes)")
    print(f"  Token savings: {savings:.1f}%")
    print(f"  Byte savings:  {savings_pct(len(json_output), len(toon_output)):.1f}%")


def main() -> None:
    parser = argparse.ArgumentParser(description="Anthropic token counting benchmark for writ")
    parser.add_argument("--model", default="claude-sonnet-4-20250514", help="Model for token counting")
    parser.add_argument("--live", action="store_true", help="Also benchmark live writ repo output")
    args = parser.parse_args()

    api_key = os.environ.get("ANTHROPIC_API_KEY")
    if not api_key:
        print("error: ANTHROPIC_API_KEY environment variable not set")
        sys.exit(1)

    client = anthropic.Anthropic(api_key=api_key)
    model = args.model

    print(f"Anthropic Token Benchmark (model: {model})")
    print("=" * 60)

    # Context benchmark
    json_ctx = generate_sample_context_json()
    toon_ctx = generate_sample_context_toon()

    json_ctx_tokens = count_tokens(client, json_ctx, model)
    toon_ctx_tokens = count_tokens(client, toon_ctx, model)
    ctx_savings = savings_pct(json_ctx_tokens, toon_ctx_tokens)

    print(f"\n--- Context (5 specs, 5 seals, 48 files) ---")
    print(f"  JSON: {json_ctx_tokens:>5} tokens ({len(json_ctx):>6} bytes)")
    print(f"  TOON: {toon_ctx_tokens:>5} tokens ({len(toon_ctx):>6} bytes)")
    print(f"  Token savings: {ctx_savings:.1f}%")
    print(f"  Byte savings:  {savings_pct(len(json_ctx), len(toon_ctx)):.1f}%")

    # Seal log benchmark
    json_seals = generate_sample_seal_log_json()
    toon_seals = generate_sample_seal_log_toon()

    json_seal_tokens = count_tokens(client, json_seals, model)
    toon_seal_tokens = count_tokens(client, toon_seals, model)
    seal_savings = savings_pct(json_seal_tokens, toon_seal_tokens)

    print(f"\n--- Seal Log (20 seals) ---")
    print(f"  JSON: {json_seal_tokens:>5} tokens ({len(json_seals):>6} bytes)")
    print(f"  TOON: {toon_seal_tokens:>5} tokens ({len(toon_seals):>6} bytes)")
    print(f"  Token savings: {seal_savings:.1f}%")
    print(f"  Byte savings:  {savings_pct(len(json_seals), len(toon_seals)):.1f}%")

    # Summary
    print(f"\n{'=' * 60}")
    print(f"Summary (Claude token counts via Anthropic API):")
    print(f"  Context: {ctx_savings:.1f}% token savings (TOON vs JSON)")
    print(f"  Seal log: {seal_savings:.1f}% token savings (TOON vs JSON)")
    print(f"  Model: {model}")
    print(f"  NOTE: These are ground-truth Claude counts, not estimates.")

    if args.live:
        run_live_benchmark(client, model)


if __name__ == "__main__":
    main()
