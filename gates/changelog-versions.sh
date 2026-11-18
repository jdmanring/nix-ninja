#!/usr/bin/env bash
# DOES EVERY CHANGELOG ROW CARRY ITS OWN VERSION, AND IS THE SEQUENCE SOUND?
#
# A version number is hand-maintained, so nothing stops two rows carrying the
# same one, or a row whose number is out of step with the row above it. Both
# make the older of the pair unreachable by name in the one table a reader looks
# a deliverable up in, and neither changes any row's text. Two rows were
# numbered the same in a single edit before this check existed.
#
# THE THREE CHECKS, each able to fail on its own:
#   unique    every row's version appears once, EXCEPT the upstream milestones
#             whose one release is described by two rows
#   ordered   the version sequence descends from the top of the table to the
#             bottom, which is what makes "newest first" true rather than
#             asserted
#   gap-free  the highest point release under the in-progress milestone is n for
#             n rows, so a renumbering that skips or repeats a number fails
#
# THE MANIFEST VERSION IS NOT COMPARED HERE. `Cargo.{toml,lock}` sit inside the
# task binary's fileset, so `Cargo.toml` names the MILESTONE and stays put until
# its exit criterion, while rows advance by point release. MEASURED 2026-11-17:
# a one-line manifest version edit reads GLOBAL RE-KEY under emission-price.sh.
# Comparing the two would demand that expensive edit for every row.
#
# NO BUILD, NO DAEMON, NO MACHINE: seconds, safe during a round.
#
#   PASS       every row's version is unique where it must be, ordered, and the
#              point releases under the current milestone run 1..n
#   FAIL       a duplicate, an out-of-order version, or a gap in the sequence
#   NO ANSWER  the changelog is absent or carries no version rows
set -uo pipefail
ROOT=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
[ -f "$ROOT/Cargo.toml" ] || { echo "NO ANSWER: no Cargo.toml above $0"; exit 2; }
cd "$ROOT" || exit 2

DOC=CHANGELOG.md
[ -f "$DOC" ] || { echo "NO ANSWER: $DOC is absent"; exit 2; }

# UPSTREAM MILESTONES MAY SHARE A VERSION, and they are named rather than
# inferred: `0.1` and `0.2` are `pdtpartners/nix-ninja`'s, where one release is
# described by two rows. Anything else duplicated is a defect.
dups=$(python3 - "$DOC" <<'PY'
import re, sys
from collections import Counter
rows = [m.group(1) for l in open(sys.argv[1], errors='replace')
        if (m := re.match(r'^\| (\d+\.\d+\.\d+) \|', l))]
c = Counter(rows)
allowed = {'0.1.0', '0.2.0'}
bad = sorted(v for v, n in c.items() if n > 1 and v not in allowed)
print('\n'.join(bad))
PY
)
if [ -n "$dups" ]; then
  while IFS= read -r v; do
    [ -n "$v" ] || continue
    echo "FAIL duplicate version   $v appears on more than one row"
  done <<DUP
$dups
DUP
  echo
  echo "FAIL: $DOC carries duplicate version numbers."
  exit 1
fi

# ORDERED AND GAP-FREE, derived together so the sequence is read once.
python3 - "$DOC" <<'PY'
import re, sys

def parse(path):
    out = []
    for l in open(path, errors='replace'):
        m = re.match(r'^\| (\d+)\.(\d+)\.(\d+) \|', l)
        if m:
            out.append(tuple(int(x) for x in m.groups()))
    return out

rows = parse(sys.argv[1])
if not rows:
    print("NO ANSWER: no version rows parsed from the changelog")
    sys.exit(2)

bad = 0
# DESCENDING, top to bottom. A version block is contiguous by construction in
# this file, so any increase is an ordering defect.
for i in range(len(rows) - 1):
    if rows[i] < rows[i + 1]:
        print(f"FAIL not descending      {'.'.join(map(str,rows[i]))} is followed by "
              f"{'.'.join(map(str,rows[i+1]))}")
        bad += 1

# GAP-FREE UNDER THE CURRENT MILESTONE: n rows numbered 1..n.
current = rows[0][:2]
pts = sorted(r[2] for r in rows if r[:2] == current)
if pts:
    expected = list(range(1, len(pts) + 1))
    if pts != expected:
        missing = sorted(set(expected) - set(pts))
        extra = [p for p in pts if p > len(pts)]
        if missing:
            print(f"FAIL gap in sequence     {'.'.join(map(str,current))}.x is missing "
                  f"{', '.join(map(str,missing))}")
            bad += 1
        if extra:
            print(f"FAIL sequence too long   {'.'.join(map(str,current))}.x reaches "
                  f"{max(pts)} over {len(pts)} rows")
            bad += 1

if bad:
    print()
    print(f"FAIL: {bad} version-sequence defect(s) in {sys.argv[1]}.")
    sys.exit(1)
print(f"OK   version sequence    {len(rows)} row(s), {len(pts)} point release(s) under "
      f"{'.'.join(map(str,current))}, unique and gap-free")
PY
rc=$?
if [ "$rc" -ne 0 ]; then
  exit "$rc"
fi
echo
echo "PASS: every changelog row carries its own version, in sequence."
exit 0
