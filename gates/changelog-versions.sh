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
#   manifest  `Cargo.toml`'s version equals the newest row's, so the two cannot
#             drift into answering a reader differently
#   dates     a row's date equals the date of the newest commit it cites, which
#             is the rule the changelog header states and nothing held until
#             2026-11-18: three rows disagreed with their own commits
#
# THE MANIFEST IS COMPARED, and that is what makes the number mean something.
# The newest row and `Cargo.toml` carry the same version, so a reader who opens
# the manifest learns where the tree is. The bump re-keys (the manifest is in
# the task binary's fileset; measured GLOBAL RE-KEY), which is why it is batched,
# not why it is skipped: an earlier version of this gate documented a split,
# and the cost of that was a manifest that answered a reader wrongly.
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
VER = re.compile(r'^\| (\d+\.\d+\.\d+) \|')
# A ROW THIS CANNOT PARSE IS NOT A ROW IT MAY IGNORE. MEASURED 2026-11-18 by
# audit: inserting `| 0.4.0-rc1 | ... |` above the newest row produced PASS,
# because the strict pattern skipped it and "newest row" stayed 0.3.105. Every
# table data row (a pipe line that is not a separator) is therefore counted,
# and one that names no three-part version is reported rather than passed over.
rows, unparsed = [], []
for l in open(sys.argv[1], errors='replace'):
    if not l.startswith('|'):
        continue
    if re.match(r'^\|[\s:|-]+\|?\s*$', l):   # separator row
        continue
    if re.match(r'^\|\s*version\s*\|\s*date\b', l, re.I):   # column header
        continue
    m = VER.match(l)
    if m:
        rows.append(m.group(1))
    else:
        unparsed.append(l.rstrip()[:70])
for u in unparsed:
    print('UNPARSED ' + u)
c = Counter(rows)
allowed = {'0.1.0', '0.2.0'}
for v in sorted(v for v, n in c.items() if n > 1 and v not in allowed):
    print('DUPLICATE ' + v)
PY
)
if [ -n "$dups" ]; then
  printf '%s\n' "$dups" | while IFS= read -r line; do
    case "$line" in
      UNPARSED*)  printf 'FAIL unparseable row      %s\n' "${line#UNPARSED }" ;;
      DUPLICATE*) printf 'FAIL duplicate version    %s appears on more than one row\n' "${line#DUPLICATE }" ;;
    esac
  done
  echo
  echo "FAIL: $DOC carries a version row this gate cannot read, or a duplicate."
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

# THE MANIFEST AND THE NEWEST ROW ARE ONE NUMBER. Read as text rather than
# parsed, since the field is a single quoted string in both places.
newest=$(python3 - "$DOC" <<'PY'
import re, sys
for l in open(sys.argv[1], errors='replace'):
    m = re.match(r'^\| (\d+\.\d+\.\d+) \|', l)
    if m:
        print(m.group(1)); break
PY
)
cargo=$(command grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')
if [ -z "$newest" ] || [ -z "$cargo" ]; then
  echo "NO ANSWER: cannot read both versions (newest row '${newest}', Cargo.toml '${cargo}')"
  exit 2
fi
if [ "$newest" != "$cargo" ]; then
  echo "FAIL manifest version    Cargo.toml says $cargo, the newest row is $newest"
  echo "  (the bump re-keys: batch it, price it, and land both in one change)"
  echo
  echo "FAIL: the manifest and the changelog disagree."
  exit 1
fi
echo "OK   manifest version    $cargo matches the newest row"

# A ROW'S DATE IS ITS NEWEST CITED COMMIT'S DATE, which the changelog header
# defines and nothing checked. Reproduced 2026-11-18 by audit: three rows
# disagreed with their own commits (0.3.89, 0.3.54, 0.3.53), and the renumbering
# rests on this definition, so a wrong date misplaces a deliverable in the only
# ordering a reader has. Skipped when git cannot resolve a hash: that is the
# citation gate's subject, and it refuses loudly there.
datebad=$(python3 - "$DOC" <<'PY'
import re, subprocess, sys
rowre = re.compile(r'^\| (\d+\.\d+\.\d+) \| (\d{4}-\d{2}-\d{2}) \|')
hashre = re.compile(r'`([0-9a-f]{7,40})`')
out = []
for n, l in enumerate(open(sys.argv[1], errors='replace'), 1):
    m = rowre.match(l)
    if not m:
        continue
    ver, date = m.group(1), m.group(2)
    ds = []
    for h in hashre.findall(l):
        r = subprocess.run(['git', 'log', '-1', '--format=%ad', '--date=short', h],
                           capture_output=True, text=True)
        if r.returncode == 0 and r.stdout.strip():
            ds.append(r.stdout.strip())
    if ds and max(ds) != date:
        out.append(f"{ver}|{date}|{max(ds)}")
print('\n'.join(out))
PY
)
if [ -n "$datebad" ]; then
  printf '%s\n' "$datebad" | while IFS='|' read -r ver said real; do
    printf 'FAIL row date             %s says %s, its newest commit is %s\n' "$ver" "$said" "$real"
  done
  echo
  echo "FAIL: a row's date is not its newest cited commit's date."
  exit 1
fi
echo "OK   row dates           each equals its newest cited commit's date"

echo
echo "PASS: every changelog row carries its own version, in sequence."
exit 0
