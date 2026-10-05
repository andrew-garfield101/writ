"""Finding 42 audit: did any writ finish on this repo drop a sealed edit?

For each finish commit C: specs whose commit_hash is C; every file any of
their seals captured. For each (spec, file), the spec's last seal blob for that
file is compared to C^ (git base) and every line it added must appear in C:file.
A missing line is SUPERSEDED if a later seal (any spec) on that file has an old
blob containing it and a new blob lacking it (a deliberate later edit);
otherwise LOST.
"""
import collections, difflib, glob, json, subprocess, sys

REPO = str(__import__("pathlib").Path(__file__).resolve().parent.parent)
W = REPO + "/.writ"

def blob(h):
    if h is None:
        return None
    try:
        raw = open(f"{W}/objects/{h[:2]}/{h[2:]}", "rb").read()
    except FileNotFoundError:
        return "MISSING"
    if raw[:1] == b"\x01":
        raw = subprocess.run(["zstd", "-dc"], input=raw[1:], capture_output=True, check=True).stdout
    elif raw[:1] == b"\x00":
        raw = raw[1:]
    return raw.decode("utf-8", "replace")

def git_file(rev, path):
    r = subprocess.run(["git", "-C", REPO, "show", f"{rev}:{path}"], capture_output=True)
    return r.stdout.decode("utf-8", "replace") if r.returncode == 0 else None

def added_lines(base, new):
    b = (base or "").splitlines(); n = new.splitlines()
    out = []
    for tag, i1, i2, j1, j2 in difflib.SequenceMatcher(None, b, n, autojunk=False).get_opcodes():
        if tag in ("insert", "replace"):
            out.extend((j, n[j]) for j in range(j1, j2))
    return out

seals = [json.load(open(f)) for f in glob.glob(f"{W}/seals/*.json")]
seals.sort(key=lambda s: s["timestamp"])
specs = {json.load(open(f))["id"]: json.load(open(f)) for f in glob.glob(f"{W}/specs/*.json")}

for c in sys.argv[1:]:
    full = subprocess.run(["git", "-C", REPO, "rev-parse", c], capture_output=True, text=True).stdout.strip()
    spec_ids = sorted(s for s, d in specs.items() if (d.get("commit_hash") or "") == full)
    committed = set(subprocess.run(["git", "-C", REPO, "show", "--name-only", "--format=", full], capture_output=True, text=True).stdout.split())
    by_file = collections.defaultdict(dict)  # file -> spec -> last seal change
    for s in seals:
        if s.get("spec_id") in spec_ids:
            for ch in s["changes"]:
                by_file[ch["path"]][s["spec_id"]] = (s, ch)
    multi = sorted(f for f, d in by_file.items() if len(d) > 1)
    print(f"=== {c} specs={spec_ids}")
    print(f"files sealed: {len(by_file)}; in commit: {len(committed)}; sealed by >1 spec: {len(multi)} {multi}")
    print(f"sealed but not in commit: {sorted(set(by_file) - committed)}")
    print(f"in commit but never sealed by these specs: {sorted(committed - set(by_file))}")
    lost = superseded = checked = 0
    for f in sorted(by_file):
        base = git_file(full + "^", f)
        final = git_file(full, f)
        for spec, (s, ch) in sorted(by_file[f].items()):
            if ch["change_type"].lower().startswith("del"):
                if final is not None:
                    print(f"  LOST-DELETE {f} spec={spec} seal={s['id'][:12]} deleted but committed present")
                    lost += 1
                continue
            content = blob(ch["new_hash"])
            if content == "MISSING":
                print(f"  UNREADABLE {f} spec={spec} seal={s['id'][:12]} blob {ch['new_hash'][:12]} missing")
                continue
            final_lines = collections.Counter((final or "").splitlines())
            for j, line in added_lines(base, content):
                checked += 1
                if final_lines[line] > 0:
                    continue
                later = [t for t in seals if t["timestamp"] > s["timestamp"]
                         for x in t["changes"] if x["path"] == f
                         and line in (blob(x["old_hash"]) or "").splitlines()
                         and line not in (blob(x["new_hash"]) or "").splitlines()]
                if later:
                    superseded += 1
                    print(f"  superseded {f}:{j+1} spec={spec} seal={s['id'][:12]} by {later[0]['id'][:12]} ({later[0].get('spec_id')}) line={line.strip()[:70]!r}")
                else:
                    lost += 1
                    print(f"  LOST {f}:{j+1} spec={spec} seal={s['id'][:12]} line={line[:100]!r}")
    print(f"added lines checked: {checked}; superseded by a later seal: {superseded}; LOST: {lost}\n")
