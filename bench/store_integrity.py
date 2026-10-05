#!/usr/bin/env python3
"""Referenced-but-missing objects in a writ store (finding 28 metric).

Live roots (the sprint 2 gc-integrity definition): every seal's tree walked
into its blobs, every spec's genesis_tree walked the same way, every change's
old_hash/new_hash, and every workspace index (.writ/workspaces/*/index.json).
Reports how many referenced objects are absent from .writ/objects/. Read-only.
The metric must be zero.

Objects are stored as <magic><payload>: 0x00 raw, 0x01 zstd, else legacy raw.
zstd payloads are decoded with the `zstd` CLI (stdlib has no zstd before 3.14).

`--allow-missing PREFIX` (repeatable) excuses specific objects known to be
permanently lost; they are reported separately and do not fail the check.

`--orphans` also lists on-disk objects outside the live set (an oracle for
`writ gc audit`'s orphaned_objects), each with its size and, when it decodes to
a JSON object, its "type" field. Orphans are reported, never a failure.

Exit: 0 = zero unexcused missing, 1 = missing objects found, 2 = cannot read.
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
from pathlib import Path

MAGIC_RAW, MAGIC_ZSTD = 0x00, 0x01


class StoreError(RuntimeError):
    """The store could not be read (as opposed to objects being missing)."""


def object_path(writ_dir: Path, h: str) -> Path:
    return writ_dir / "objects" / h[:2] / h[2:]


def read_object(writ_dir: Path, h: str) -> bytes:
    raw = object_path(writ_dir, h).read_bytes()
    if not raw:
        return raw
    if raw[0] == MAGIC_RAW:
        return raw[1:]
    if raw[0] == MAGIC_ZSTD:
        if not shutil.which("zstd"):
            raise StoreError("zstd CLI not found; cannot decode tree objects")
        proc = subprocess.run(["zstd", "-dc"], input=raw[1:], capture_output=True)
        if proc.returncode != 0:
            raise StoreError(f"zstd failed on {h[:12]}: {proc.stderr.decode().strip()}")
        return proc.stdout
    return raw


def orphan_report(writ_dir: Path, referenced: set[str]) -> list[dict]:
    """On-disk objects not in the referenced set, with size and JSON "type"."""
    out = []
    for prefix in sorted((writ_dir / "objects").iterdir()):
        if not prefix.is_dir() or len(prefix.name) != 2:
            continue
        for obj in sorted(prefix.iterdir()):
            h = prefix.name + obj.name
            if h in referenced:
                continue
            try:
                doc = json.loads(read_object(writ_dir, h))
                kind = doc.get("type") if isinstance(doc, dict) else None
            except (ValueError, UnicodeDecodeError):
                kind = None
            out.append({"hash": h[:12], "bytes": obj.stat().st_size, "type": kind})
    return out


def check(writ_dir: Path, allow: tuple[str, ...] = (), orphans: bool = False) -> dict:
    seals_dir = writ_dir / "seals"
    if not seals_dir.is_dir():
        raise StoreError(f"no seals dir at {seals_dir}")
    referenced: set[str] = set()
    missing_trees: list[str] = []
    seals = 0
    for seal_file in sorted(seals_dir.glob("*.json")):
        seal = json.loads(seal_file.read_text())
        seals += 1
        for change in seal.get("changes", []):
            for key in ("old_hash", "new_hash"):
                if change.get(key):
                    referenced.add(change[key])
        tree = seal.get("tree")
        if not tree:
            continue
        referenced.add(tree)
        if not object_path(writ_dir, tree).exists():
            missing_trees.append(f"{seal['id'][:12]}:{tree[:12]}")
            continue
        entries = json.loads(read_object(writ_dir, tree) or b"{}")
        referenced.update(e["hash"] for e in entries.values() if e.get("hash"))
    genesis_trees = 0
    for spec_file in sorted((writ_dir / "specs").glob("*.json")):
        tree = json.loads(spec_file.read_text()).get("genesis_tree")
        if not tree:
            continue
        genesis_trees += 1
        referenced.add(tree)
        if not object_path(writ_dir, tree).exists():
            missing_trees.append(f"spec {spec_file.stem}:{tree[:12]}")
            continue
        entries = json.loads(read_object(writ_dir, tree) or b"{}")
        referenced.update(e["hash"] for e in entries.values() if e.get("hash"))
    index_refs = 0
    for index_file in sorted((writ_dir / "workspaces").glob("*/index.json")):
        entries = json.loads(index_file.read_text()).get("entries", {})
        for e in entries.values():
            if e.get("hash"):
                referenced.add(e["hash"])
                index_refs += 1
    absent = sorted(h for h in referenced if not object_path(writ_dir, h).exists())
    allowed = [h for h in absent if any(h.startswith(a) for a in allow)]
    missing = [h for h in absent if h not in allowed]
    extra = {"orphans": orphan_report(writ_dir, referenced)} if orphans else {}
    return {
        "seals": seals,
        "referenced_objects": len(referenced),
        "index_references": index_refs,
        "genesis_trees": genesis_trees,
        "missing_objects": len(missing),
        "missing_trees": missing_trees,
        "missing_sample": [h[:12] for h in missing[:10]],
        "allowed_missing": [h[:12] for h in allowed],
        "unused_allowances": [a for a in allow if not any(h.startswith(a) for h in allowed)],
        **extra,
    }


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("repo", type=Path, help="project root containing .writ/")
    ap.add_argument("--json", action="store_true", help="print JSON")
    ap.add_argument("--allow-missing", action="append", default=[], metavar="PREFIX",
                    help="hash prefix (>= 12 hex) of a known-lost object to excuse")
    ap.add_argument("--orphans", action="store_true",
                    help="also list on-disk objects outside the live set")
    args = ap.parse_args(argv)
    try:
        if any(len(a) < 12 for a in args.allow_missing):
            raise StoreError("--allow-missing prefixes must be at least 12 characters")
        result = check(args.repo / ".writ", tuple(args.allow_missing), args.orphans)
    except (StoreError, OSError, json.JSONDecodeError) as exc:
        print(f"store_integrity: {exc}", file=sys.stderr)
        return 2
    if args.json:
        print(json.dumps(result, indent=2))
    else:
        print(f"seals {result['seals']}, referenced {result['referenced_objects']}, "
              f"missing {result['missing_objects']}, allowed {len(result['allowed_missing'])}"
              + (f", orphans {len(result['orphans'])}" if args.orphans else ""))
    return 1 if result["missing_objects"] else 0


if __name__ == "__main__":
    sys.exit(main())
