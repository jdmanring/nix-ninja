#!/usr/bin/env python3
"""Verify docs/upstream/ledger.tsv against the repository.

"Is every deviation from upstream staged?" is a claim about a COUNT, and prose
cannot hold one - the offers table in docs/upstream/README.md was written from
intent and drifted from the diff without anything noticing. This checks the
ledger against reality and fails when they disagree.

Three questions, and it answers all three every run:

  1. COVERAGE  - does every path that differs from upstream/main belong to a
                 ledger item? An unclaimed path is a contribution nobody has
                 decided the destination of.
  2. BRANCHES  - does every branch the ledger names actually exist?
  3. DRAFTS    - does every draft the ledger names actually exist?

Exit 0 when all three hold, 1 otherwise, so it can gate a commit.

    scripts/upstream-ledger.py            # full report
    scripts/upstream-ledger.py --summary  # counts only
"""
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LEDGER = ROOT / "docs" / "upstream" / "ledger.tsv"
BASE = "upstream/main"


def sh(*args):
    """Run a git command. Captures status separately from output: an empty
    result and a failed command are different facts, and conflating them is
    how a coverage check reports a clean sheet because git errored."""
    p = subprocess.run(["git", "-C", str(ROOT), *args],
                       capture_output=True, text=True)
    if p.returncode != 0:
        print(f"FATAL: git {' '.join(args)} failed: {p.stderr.strip()}",
              file=sys.stderr)
        sys.exit(2)
    return p.stdout


def load_ledger():
    if not LEDGER.exists():
        print(f"FATAL: no ledger at {LEDGER}", file=sys.stderr)
        sys.exit(2)
    items = []
    for line in LEDGER.read_text().splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        parts = line.split("\t")
        if len(parts) != 7:
            print(f"FATAL: ledger row has {len(parts)} fields, want 7: "
                  f"{line[:60]}", file=sys.stderr)
            sys.exit(2)
        iid, title, dest, branch, draft, status, paths = parts
        items.append({
            "id": iid, "title": title, "destination": dest,
            "branch": None if branch == "-" else branch,
            "draft": None if draft == "-" else draft,
            "status": status,
            "paths": [] if paths == "-" else paths.split(";"),
        })
    return items


def deviating_paths():
    out = sh("diff", "--name-only", f"{BASE}..HEAD")
    return [p for p in out.splitlines() if p.strip()]


def claims(path, patterns):
    """A ledger path claims a file if it matches exactly or is a directory
    prefix. Prefix matching is deliberate: vendor-n2/ is one item and listing
    its 60 files would be a ledger nobody maintains."""
    for pat in patterns:
        if path == pat:
            return True
        if pat.endswith("/") and path.startswith(pat):
            return True
    return False


def main():
    summary_only = "--summary" in sys.argv
    items = load_ledger()
    paths = deviating_paths()

    # 1. coverage
    unclaimed = []
    for p in paths:
        if not any(claims(p, it["paths"]) for it in items):
            unclaimed.append(p)

    # 2. branches
    local = set(sh("branch", "--format=%(refname:short)").split())
    remote = set(sh("branch", "-r", "--format=%(refname:short)").split())
    missing_branches = []
    for it in items:
        b = it["branch"]
        if b and b not in local and f"origin/{b}" not in remote:
            missing_branches.append((it["id"], b))

    # 3. drafts
    missing_drafts = []
    for it in items:
        d = it["draft"]
        if d and not (ROOT / "docs" / "upstream" / d).exists():
            missing_drafts.append((it["id"], d))

    by_status = {}
    for it in items:
        by_status[it["status"]] = by_status.get(it["status"], 0) + 1

    print(f"ledger items: {len(items)}   deviating paths: {len(paths)}")
    print("status: " + ", ".join(f"{k}={v}" for k, v in sorted(by_status.items())))

    if not summary_only:
        print()
        print(f"{'id':<4}{'status':<18}{'branch':<24}{'draft':<32}title")
        for it in items:
            print(f"{it['id']:<4}{it['status']:<18}"
                  f"{(it['branch'] or '-'):<24}{(it['draft'] or '-'):<32}"
                  f"{it['title']}")

    ok = True
    if unclaimed:
        ok = False
        print(f"\nUNCLAIMED PATHS ({len(unclaimed)}) - deviating from upstream "
              f"and owned by no ledger item:")
        for p in unclaimed:
            print(f"  {p}")
    if missing_branches:
        ok = False
        print(f"\nMISSING BRANCHES ({len(missing_branches)}):")
        for iid, b in missing_branches:
            print(f"  item {iid}: {b}")
    if missing_drafts:
        ok = False
        print(f"\nMISSING DRAFTS ({len(missing_drafts)}):")
        for iid, d in missing_drafts:
            print(f"  item {iid}: {d}")

    if ok:
        print("\nOK: every deviating path is claimed, every branch and draft "
              "named by the ledger exists.")
        print("NOTE: this proves the ledger is CONSISTENT, never that an item "
              "is ready to send. Readiness is each draft's own audit block.")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
