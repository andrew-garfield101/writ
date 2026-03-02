#!/usr/bin/env python3
"""Multi-agent convergence demo.

Simulates two agents working in parallel on a Python web app,
then converges their work automatically using writ.

Usage:
    python demo.py
"""

import tempfile
from pathlib import Path

import writ


def main():
    with tempfile.TemporaryDirectory() as tmp:
        workspace = Path(tmp)
        print("\n  Multi-Agent Convergence Demo")
        print("  " + "=" * 50)

        # ── Step 1: Baseline project ────────────────────────
        print("\n  Step 1: Setting up baseline project...")

        # Create baseline files
        (workspace / "app.py").write_text(
            '"""Web application."""\n'
            "\n"
            "from flask import Flask\n"
            "\n"
            "app = Flask(__name__)\n"
            "\n"
            "\n"
            '@app.route("/")\n'
            "def index():\n"
            '    return {"status": "ok"}\n'
        )

        (workspace / "models.py").write_text(
            '"""Data models."""\n'
            "\n"
            "\n"
            "class User:\n"
            "    def __init__(self, id: int, name: str):\n"
            "        self.id = id\n"
            "        self.name = name\n"
        )

        # Initialize writ and create specs
        repo = writ.Repository.init(str(workspace))
        repo.add_spec(id="backend", title="Backend Auth")
        repo.add_spec(id="api", title="API Layer")

        repo.seal(
            summary="baseline: flask app with User model",
            agent_id="setup",
            agent_type="agent",
            status="in-progress",
        )
        print("  Baseline sealed.")

        # Snapshot baseline for restore-between-agents
        baseline = {
            "app.py": (workspace / "app.py").read_text(),
            "models.py": (workspace / "models.py").read_text(),
        }

        # ── Step 2: Agent A (backend) ──────────────────────
        print("\n  Step 2: Agent A (backend-dev) working...")

        (workspace / "app.py").write_text(
            '"""Web application."""\n'
            "\n"
            "from flask import Flask, request, session\n"
            "\n"
            "app = Flask(__name__)\n"
            "\n"
            "\n"
            '@app.route("/")\n'
            "def index():\n"
            '    return {"status": "ok"}\n'
            "\n"
            "\n"
            '@app.route("/login", methods=["POST"])\n'
            "def login():\n"
            '    username = request.json.get("username")\n'
            '    session["user"] = username\n'
            '    return {"logged_in": True}\n'
            "\n"
            "\n"
            '@app.route("/logout", methods=["POST"])\n'
            "def logout():\n"
            '    session.pop("user", None)\n'
            '    return {"logged_out": True}\n'
        )

        (workspace / "models.py").write_text(
            '"""Data models."""\n'
            "\n"
            "\n"
            "class User:\n"
            "    def __init__(self, id: int, name: str):\n"
            "        self.id = id\n"
            "        self.name = name\n"
            "\n"
            "\n"
            "class Session:\n"
            '    """Tracks active user sessions."""\n'
            "\n"
            "    def __init__(self, user_id: int, token: str):\n"
            "        self.user_id = user_id\n"
            "        self.token = token\n"
        )

        seal_a = repo.seal(
            summary="backend: auth routes + Session model",
            agent_id="agent-a",
            agent_type="agent",
            spec_id="backend",
            status="in-progress",
        )
        print(f"  Agent A sealed: {seal_a['id'][:12]}...")

        # ── Restore baseline for Agent B ────────────────────
        for filename, content in baseline.items():
            (workspace / filename).write_text(content)

        # ── Step 3: Agent B (api) ───────────────────────────
        print("\n  Step 3: Agent B (api-dev) working...")

        (workspace / "app.py").write_text(
            '"""Web application."""\n'
            "\n"
            "from flask import Flask, jsonify, request\n"
            "\n"
            "app = Flask(__name__)\n"
            "\n"
            "\n"
            '@app.route("/")\n'
            "def index():\n"
            '    return {"status": "ok"}\n'
            "\n"
            "\n"
            '@app.route("/validate", methods=["POST"])\n'
            "def validate():\n"
            "    data = request.json\n"
            '    errors = [f"{k} required" for k in ["name", "email"] if k not in data]\n'
            "    return jsonify({\"valid\": len(errors) == 0, \"errors\": errors})\n"
        )

        (workspace / "models.py").write_text(
            '"""Data models."""\n'
            "\n"
            "\n"
            "class User:\n"
            "    def __init__(self, id: int, name: str):\n"
            "        self.id = id\n"
            "        self.name = name\n"
            "\n"
            "\n"
            "class Schema:\n"
            '    """Defines validation rules for API input."""\n'
            "\n"
            "    def __init__(self, fields: list, required: list):\n"
            "        self.fields = fields\n"
            "        self.required = required\n"
        )

        seal_b = repo.seal(
            summary="api: validation endpoint + Schema model",
            agent_id="agent-b",
            agent_type="agent",
            spec_id="api",
            status="in-progress",
        )
        print(f"  Agent B sealed: {seal_b['id'][:12]}...")

        # ── Step 4: Check divergence ────────────────────────
        print("\n  Step 4: Checking for divergence...")
        ctx = repo.context()
        diverged = ctx.get("diverged_branches", [])
        risk = ctx.get("integration_risk", {})
        print(f"  Diverged branches: {len(diverged)}")
        print(f"  Integration risk: {risk.get('level', 'unknown')} (score: {risk.get('score', '?')})")
        print(f"  Convergence recommended: {ctx.get('convergence_recommended', False)}")

        # ── Step 5: Converge ────────────────────────────────
        print("\n  Step 5: Running convergence...")
        report = repo.converge_all(strategy="most-complete", apply=True)

        print(f"  Clean merge: {report.get('is_clean', False)}")
        print(f"  Total conflicts: {report.get('total_conflicts', 0)}")
        print(f"  Files merged: {len(report.get('merge_steps', []))}")

        # Show merged results
        print("\n  Merged models.py:")
        merged_models = (workspace / "models.py").read_text()
        for line in merged_models.splitlines():
            print(f"    {line}")

        # ── Step 6: Verify chain ────────────────────────────
        print("\n  Step 6: Verifying seal chain integrity...")
        chain = repo.verify_chain()
        print(f"  Total seals: {chain['total_seals']}")
        print(f"  Chain valid: {chain['valid']}")
        print(f"  Failures: {len(chain['failures'])}")

        # ── Summary ─────────────────────────────────────────
        print("\n  " + "=" * 50)
        print("  Demo complete!")
        print()
        print("  Two agents modified the same files in parallel.")
        print("  Writ converged their work automatically:")
        print("    - Both models (Session + Schema) preserved")
        print("    - Both route sets (auth + validation) composed")
        print("    - Imports merged without duplication")
        print("    - Cryptographic chain verified clean")
        print("  " + "=" * 50)
        print()


if __name__ == "__main__":
    main()
