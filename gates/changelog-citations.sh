#!/usr/bin/env bash
# DOES EVERY COMMIT A CHANGELOG ROW CITES STILL RESOLVE?
#
# A row's hashes are its evidence, and this tree rewrites history. A hash that
# survived a rewrite on a developer's machine does not reach a reader: it is an
# object nobody can fetch, so the row reads as checked while the evidence is
# gone. MEASURED 2026-11-16: 106 rows cited 220 distinct hashes and one of them,
# `5f7a58d`, resolved in no branch and in no remote.
#
# THE QUESTION IS RESOLVABILITY, and the subject is deliberately NOT compared.
# A first version compared each hash against the row's prose and passed every
# row, because the row quotes the subject somewhere in its own text: the check
# could not have failed, so it was removed rather than left in. A row pointing
# at the wrong-but-existing commit is caught by reading the row, not by this.
#
# WHAT IT CANNOT SEE: an amended commit whose old object was garbage collected
# looks identical to a typo, and a hash belonging to the sibling repository
# (the corpus lives in `local/`) is absent here by design. Both are why the
# message names the row instead of asserting which repository it meant.
#
# Usage: changelog-citations.sh [REV]
#   No argument reads the WORKING TREE, which is what catches a bad hash before
#   it lands; a revision reads that revision's file, for asking what an older
#   tree looked like. Reading only by revision was the first version and it
#   passed a planted bad hash, because the edit was not committed yet.
#   PASS  every cited hash in a row resolves in this repository
#   FAIL  a hash does not resolve
#   NO ANSWER  the changelog cannot be read
set -u
REV=${1:-}
# SELF-LOCATING, the way the published gates are: walk up to the manifest rather
# than assume a depth, because the private corpus reaches the same file through a
# symlink one level deeper and a published copy sits at another depth again.
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
[ -f "$REPO/Cargo.toml" ] || { echo "NO ANSWER: no Cargo.toml above $0"; exit 2; }
cd "$REPO" || exit 2
if [ -n "$REV" ]; then
  doc=$(git show "$REV:CHANGELOG.md" 2>/dev/null) || { echo "NO ANSWER: no CHANGELOG.md at $REV"; exit 2; }
  where=$REV
else
  doc=$(cat CHANGELOG.md 2>/dev/null) || { echo "NO ANSWER: cannot read CHANGELOG.md here"; exit 2; }
  where="the working tree"
fi
[ -n "$doc" ] || { echo "NO ANSWER: CHANGELOG.md at $where reads empty"; exit 2; }

rows=0 bad=0
while IFS= read -r line; do
  case $line in '| 0.'*) ;; *) continue ;; esac
  rows=$((rows+1))
  for h in $(printf '%s' "$line" | command grep -oE '`[0-9a-f]{7,40}`' | tr -d '`' | sort -u); do
    git cat-file -e "$h^{commit}" 2>/dev/null && continue
    printf '  UNRESOLVABLE  %s  (cited in the row dated %s)\n' "$h" "$(printf '%s' "$line" | cut -d'|' -f3 | tr -d ' ')"
    bad=$((bad+1))
  done
done <<EOF
$doc
EOF
[ "$rows" -gt 0 ] || { echo "NO ANSWER: no changelog rows found in $where"; exit 2; }
if [ "$bad" -eq 0 ]; then
  echo "PASS: every hash cited by a changelog row resolves in $where ($rows rows)"
  exit 0
fi
echo "FAIL: $bad cited hash(es) do not resolve; a row naming one reads as checked"
exit 1
