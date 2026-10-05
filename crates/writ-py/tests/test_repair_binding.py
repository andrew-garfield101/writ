"""S.7 `writ repair` through the Python binding, on a deliberately damaged store."""

from pathlib import Path

import writ


def _sealed_repo(tmp_path: Path) -> tuple[writ.Repository, Path]:
    repo = writ.Repository.init(str(tmp_path))
    (tmp_path / "a.txt").write_text("recover me\n")
    seal = repo.seal(summary="a", agent_id="a", agent_type="agent")
    blob = next(c["new_hash"] for c in seal["changes"] if c["path"] == "a.txt")
    obj = tmp_path / ".writ" / "objects" / blob[:2] / blob[2:]
    assert obj.exists(), obj
    return repo, obj


def test_repair_on_a_clean_store_reports_clean(tmp_path):
    repo, _ = _sealed_repo(tmp_path)

    report = repo.repair()

    assert report["is_clean"] is True
    assert report["missing"] == []


def test_dry_run_reports_recoverable_and_writes_nothing(tmp_path):
    repo, obj = _sealed_repo(tmp_path)
    obj.unlink()

    report = repo.repair(dry_run=True)

    assert report["dry_run"] is True
    assert [r["path"] for r in report["recovered"]] == ["a.txt"]
    assert report["recovered"][0]["source"]["kind"] == "working_tree"
    assert report["is_clean"] is True
    assert not obj.exists()


def test_repair_regenerates_the_missing_object_from_the_working_tree(tmp_path):
    repo, obj = _sealed_repo(tmp_path)
    obj.unlink()

    report = repo.repair()

    assert report["is_clean"] is True
    assert obj.exists()
    assert repo.repair()["missing"] == []


def test_unrecoverable_object_is_reported_not_clean(tmp_path):
    repo, obj = _sealed_repo(tmp_path)
    obj.unlink()
    (tmp_path / "a.txt").write_text("changed, so the sealed content is gone\n")

    report = repo.repair()

    assert report["is_clean"] is False
    assert [u["path"] for u in report["unrecoverable"]] == ["a.txt"]
    assert not obj.exists()
