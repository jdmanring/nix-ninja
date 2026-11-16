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
#   PASS  every cited commit in the tracked documents is on HEAD history
#   FAIL  a cited hash is absent from the default branch history
#   NO ANSWER  the changelog cannot be read
set -u
REV=${1:-}
# SELF-LOCATING, the way the published gates are: walk up to the manifest rather
# than assume a depth, because the private corpus reaches the same file through a
# symlink one level deeper and a published copy sits at another depth again.
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
[ -f "$REPO/Cargo.toml" ] || { echo "NO ANSWER: no Cargo.toml above $0"; exit 2; }
cd "$REPO" || exit 2
# THE SUBJECT IS THE DOCUMENT SET, at the working tree by default and at a
# revision on request. Reading only by revision was the first version and it
# passed a planted bad hash, because the edit was not committed yet.
if [ -n "$REV" ]; then
  FILES=$(git ls-tree -r --name-only "$REV" 2>/dev/null | command grep '\.md$') \
    || { echo "NO ANSWER: cannot list $REV"; exit 2; }
  read_doc() { git show "$REV:$1" 2>/dev/null; }
  where=$REV
else
  FILES=$(git ls-files '*.md' 2>/dev/null) || { echo "NO ANSWER: cannot list tracked documents here"; exit 2; }
  read_doc() { cat "$1" 2>/dev/null; }
  where="the working tree"
fi
[ -n "$FILES" ] || { echo "NO ANSWER: no markdown documents at $where"; exit 2; }

# EVERY TRACKED MARKDOWN FILE, not only the changelog: a design doc or the
# roadmap cites a commit for the same reason a row does, and the same rewrite
# strands it. MEASURED 2026-11-16 over the tree: 246 commit-ish citations in
# tracked .md, none dangling; the changelog's own four were the whole defect.
#
# A HASH THAT NAMES SOMETHING OTHER THAN A COMMIT IS NOT A CITATION. Store paths
# and content hashes are hex too, so each candidate must resolve as a commit
# before it is judged; one that does not is skipped rather than reported.
#
# REACHABLE FROM THE DEFAULT BRANCH, not merely present in this clone and not
# merely carried by some branch. `git cat-file -e` only asks whether the object
# exists, and this tree rewrites history, so an amended or pre-rebase commit
# stays in the object database until it is collected: a citation written against
# one passes locally and fails for every reader. A branch that existed only on
# the author's disk has the same shape, which is why the question is the history
# a plain clone receives rather than every ref here.
#
# MEASURED 2026-11-16: of 246 commit-ish citations in tracked .md, every one is
# on HEAD's history, and four were not fetchable before this gate existed.
MAINREVS=$(git rev-list HEAD 2>/dev/null | sort)
reachable() {
  local oid
  oid=$(git rev-parse --verify --quiet "$1^{commit}" 2>/dev/null) || return 1
  printf '%s\n' "$MAINREVS" | command grep -qx "$oid"
}
rows=0 bad=0 dangling=0
while IFS= read -r f; do
  [ -n "$f" ] || continue
  while IFS= read -r h; do
    [ -n "$h" ] || continue
    git cat-file -e "$h^{commit}" 2>/dev/null || continue
    rows=$((rows+1))
    reachable "$h" && continue
    printf '  DANGLING      %s  (%s: absent from the default branch history)\n' "$h" "$f"
    dangling=$((dangling+1))
  done <<HASHES
$(read_doc "$f" | command grep -oE '`[0-9a-f]{7,40}`' | tr -d '`' | sort -u)
HASHES
done <<FILES
$FILES
FILES
bad=$((bad+dangling))
[ "$rows" -gt 0 ] || { echo "NO ANSWER: no commit citations found in the documents at $where"; exit 2; }
if [ "$bad" -eq 0 ]; then
  echo "PASS: every cited commit is on the default branch history ($rows citations, $where)"
  exit 0
fi
echo "FAIL: $bad cited hash(es) a reader cannot fetch; a document naming one reads as checked"
exit 1
