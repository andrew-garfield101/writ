"""Contract tests for bench/verify_check.py's classifier (pure, no binary)."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from verify_check import classify  # noqa: E402

A = "ae01d21c123e" + "0" * 52
B = "50ff4a66d3af" + "1" * 52
OTHER = "deadbeef0000" + "2" * 52
ALLOW = ("ae01d21c123e", "50ff4a66d3af")


def report(missing=(), unreadable=(), head_valid=True, spec_valid=True, all_valid=None):
    if all_valid is None:
        all_valid = not missing and not unreadable and head_valid and spec_valid
    return {
        "all_valid": all_valid,
        "head_chain": {"valid": head_valid, "failures": [] if head_valid else ["x"]},
        "spec_chains": [{"spec_id": "s1", "valid": spec_valid,
                         "failures": [] if spec_valid else ["y"]}],
        "missing_objects": [{"hash": h, "path": "p"} for h in missing],
        "unreadable_trees": list(unreadable),
    }


def test_clean_store_passes():
    assert classify(report())["verdict"] == "PASS"


def test_only_allow_listed_missing_is_expected():
    r = classify(report(missing=[A, B]), ALLOW)
    assert r["verdict"] == "EXPECTED"
    assert r["problems"] == []


def test_unlisted_missing_object_fails():
    r = classify(report(missing=[A, B, OTHER]), ALLOW)
    assert r["verdict"] == "FAIL"
    assert "deadbeef0000" in r["problems"][0]


def test_stale_allowance_fails():
    r = classify(report(missing=[A]), ALLOW)
    assert r["verdict"] == "FAIL"
    assert "50ff4a66d3af" in r["problems"][0]


def test_chain_failure_fails_even_when_missing_are_excused():
    r = classify(report(missing=[A, B], spec_valid=False), ALLOW)
    assert r["verdict"] == "FAIL"
    assert any("chain s1" in p for p in r["problems"])


def test_unreadable_tree_fails():
    r = classify(report(missing=[A, B], unreadable=[{"hash": OTHER}]), ALLOW)
    assert r["verdict"] == "FAIL"


def test_all_valid_false_with_no_recognized_failure_fails():
    assert classify(report(all_valid=False))["verdict"] == "FAIL"


def test_missing_without_allowances_fails():
    assert classify(report(missing=[A]))["verdict"] == "FAIL"
