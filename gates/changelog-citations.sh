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
#   PASS  every cited commit in the tracked documents is on the default branch
#   FAIL  a cited hash is absent from the default branch history
#   NO ANSWER  the changelog cannot be read, or the branch cannot be resolved
set -u
REV=${1:-}
# SELF-LOCATING, the way the published gates are: walk up to the manifest rather
# than assume a depth, because the private corpus reaches the same file through a
# symlink one level deeper and a published copy sits at another depth again.
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
[ -f "$REPO/Cargo.toml" ] || { echo "NO ANSWER: no Cargo.toml above $0"; exit 2; }
cd "$REPO" || exit 2
# THE QUESTION NAMES THE DEFAULT BRANCH, SO THE ANSWER IS READ FROM IT. The old
# body measured `rev-list HEAD` while PRINTING "the default branch history". In
# this checkout the two are the same object, so the substitution changed no
# verdict here; it is wrong in any tree where they differ. A topic branch is the
# normal state of a pull request and of CI's merge checkout, and there a row
# citing a commit a squash-merge would strand passed on the very PR that
# introduced it (reproduced 2026-11-16 by audit in a clone). A branch that
# cannot be resolved is a NO ANSWER: guessing HEAD would answer a different
# question than the one the verdict text claims.
# A DANGLING SYMREF IS NOT AN ANSWER, and it used to become a mass FAIL.
# REPRODUCED 2026-11-16 by audit: after a default branch is renamed on the forge
# and `git fetch --prune` drops the old name, `origin/HEAD` still names it. The
# symref command exits 0, `rev-list` returns nothing, and every row was reported
# DANGLING - here `FAIL: 249 cited hash(es) a reader cannot fetch` over hashes
# that all fetch fine. Each candidate is therefore VALIDATED by rev-parse before
# it is accepted, rather than trusted because a command exited 0.
if DEFAULT_REF=$(git symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null) \
   && git rev-parse --verify --quiet "$DEFAULT_REF" >/dev/null 2>&1; then
  :
elif git rev-parse --verify --quiet refs/remotes/origin/main >/dev/null 2>&1; then
  # `actions/checkout` on a pull request leaves a detached HEAD and need not
  # create `origin/HEAD` or a local branch, but `origin/main` is there. Without
  # this rung the gate would refuse in the one configuration CI actually runs.
  DEFAULT_REF=origin/main
elif git rev-parse --verify --quiet refs/heads/main >/dev/null 2>&1; then
  DEFAULT_REF=main
else
  echo "NO ANSWER: cannot resolve the default branch (no usable origin/HEAD, no origin/main, no local main)"
  exit 2
fi
# A TRUNCATED HISTORY CANNOT ANSWER A REACHABILITY QUESTION, and it used to
# answer PASS. MEASURED 2026-11-16 in a clone carrying 409 of 1082 commits: the
# gate reported `PASS ... (188 citations)` and the full tree reports 249, so it
# discarded 61 citations without a word. Two causes, and the shallow repository
# was only the visible one: a hash whose object this clone lacks was skipped as
# "not a citation", and the branch it compared against was HEAD. A shallow
# repository is refused outright rather than patched per hash, because every
# hash it holds is judged against a history that is missing the very commits the
# question is about. The refusal REQUIRES THE ANSWER `false`: an unanswered
# question is not a `false`, and a git too old to know the option reported PASS.
SHALLOW=$(git rev-parse --is-shallow-repository 2>/dev/null)
if [ "$SHALLOW" != false ]; then
  echo "NO ANSWER: shallow clone (or unreadable: '${SHALLOW:-empty}'); reachability against a truncated history is not a verdict"
  echo "  (fetch the full history: git fetch --unshallow)"
  exit 2
fi

# THE SUBJECT IS ONE SET FOR BOTH THE LIST AND THE READ. REPRODUCED 2026-11-16 by
# audit: the list was `git ls-files` (tracked only) while the read was `cat` (the
# working tree), so a document one `git add` away from being tracked was skipped
# entirely and a planted bad hash in it PASSED - the exact window the header says
# this mode exists to cover. Untracked-but-not-ignored documents are now part of
# the subject, so an edit is judged before it is committed. Build artefacts and
# scratch trees stay out because `--exclude-standard` honours the ignore files.
if [ -n "$REV" ]; then
  FILES=$(git ls-tree -r --name-only "$REV" 2>/dev/null | command grep '\.md$') \
    || { echo "NO ANSWER: cannot list $REV"; exit 2; }
  read_doc() { git show "$REV:$1" 2>/dev/null; }
  where=$REV
else
  FILES=$( { git ls-files '*.md'; git ls-files --others --exclude-standard '*.md'; } 2>/dev/null ) \
    || { echo "NO ANSWER: cannot list the working tree's documents"; exit 2; }
  read_doc() { cat "$1" 2>/dev/null; }
  where="the working tree"
fi
[ -n "$FILES" ] || { echo "NO ANSWER: no markdown documents at $where"; exit 2; }

# EVERY TRACKED MARKDOWN FILE, not only the changelog: a design doc or the
# roadmap cites a commit for the same reason a row does, and the same rewrite
# strands it. MEASURED 2026-11-16 over the tree: no commit citation in tracked
# .md was dangling; the changelog's own four were the whole defect. The count
# lives in the run's own output rather than here, because a number written into
# a comment goes stale the next time a document cites a commit.
#
# A HASH THAT NAMES SOMETHING OTHER THAN A COMMIT IS NOT A CITATION. Store paths
# and content hashes are hex too, so each candidate must resolve as a commit
# before it is judged; one that does not is skipped rather than reported.
#
# THE CANDIDATES ARE BACKTICKED **AND** BARE, because the documents cite both
# ways. REPRODUCED 2026-11-16 by audit: the evidence column writes
# `emission-price.sh 66193ed`, so a bare-citation-only rule left eight real
# commits in this tree unjudged, and a side-branch commit cited bare PASSED
# where the same commit backticked FAILED. A bare word needs a tighter shape than
# a backticked one, since ordinary hex-coloured prose must not become a citation:
# `\b` at both ends, and the token is still required to resolve as a commit
# before it counts.
#
# REACHABLE FROM THE DEFAULT BRANCH, not merely present in this clone and not
# merely carried by some branch. `git cat-file -e` only asks whether the object
# exists, and this tree rewrites history, so an amended or pre-rebase commit
# stays in the object database until it is collected: a citation written against
# one passes locally and fails for every reader. A branch that existed only on
# the author's disk has the same shape, which is why the question is the history
# a plain clone receives rather than every ref here.
#
# MEASURED 2026-11-16: no citation in tracked .md was off the default branch's
# history, and four were not fetchable before this gate existed.
# THE LIST IS CAPTURED UNPIPED, because `cmd | sort` hands back the status of
# `sort`: an empty history read as a successful one and the guard below never
# fired. A `-z` test on the result is the second half of the same fix, since an
# empty branch and an unreadable one must not both read as "nothing is dangling".
MAINREVS=$(git rev-list "$DEFAULT_REF" 2>/dev/null); rc=$?
[ "$rc" -eq 0 ] && [ -n "$MAINREVS" ] || {
  echo "NO ANSWER: cannot list $DEFAULT_REF (rc $rc, ${#MAINREVS} bytes)"; exit 2; }
MAINREVS=$(printf '%s\n' "$MAINREVS" | sort)
reachable() {
  local oid
  oid=$(git rev-parse --verify --quiet "$1^{commit}" 2>/dev/null) || return 1
  printf '%s\n' "$MAINREVS" | command grep -qx "$oid"
}
# THE SKIP IS NECESSARY AND WAS INVISIBLE. A hash-shaped token that names no
# commit is not a citation - the tree holds three, all legitimate (two nixpkgs
# commits and a commit in upstream nix quoted from a patch header) - so the loop
# must pass over them. But an unreported skip cannot be told apart from a
# citation this clone lacks, and REPRODUCED 2026-11-16 by audit: a planted
# `deadbeeff00d` gave the same PASS as a clean tree. The count is now printed on
# every exit, so it is readable beside a FAIL too.
rows=0 bad=0 dangling=0 skipped=0
while IFS= read -r f; do
  [ -n "$f" ] || continue
  while IFS= read -r h; do
    [ -n "$h" ] || continue
    git cat-file -e "$h^{commit}" 2>/dev/null || { skipped=$((skipped+1)); continue; }
    rows=$((rows+1))
    reachable "$h" && continue
    printf '  DANGLING      %s  (%s: absent from the default branch history)\n' "$h" "$f"
    dangling=$((dangling+1))
  done <<HASHES
$(read_doc "$f" | command grep -oE '`[0-9a-f]{7,40}`|\b[0-9a-f]{7,40}\b' | tr -d '`' | sort -u)
HASHES
done <<FILES
$FILES
FILES
bad=$((bad+dangling))
[ "$rows" -gt 0 ] || { echo "NO ANSWER: no commit citations found in the documents at $where"; exit 2; }
# THE SKIP COUNT IS A COUNT OF SKIPS, not of distinct tokens: the same hash
# planted in two documents moves it by two. Named so a reader cannot read it as
# a population of tokens.
summary="checked against $DEFAULT_REF: $rows citations, $skipped non-commit skips, $where"
if [ "$bad" -eq 0 ]; then
  echo "PASS: every cited commit is on the default branch history ($summary)"
  exit 0
fi
echo "  ($summary)"
echo "FAIL: $bad cited hash(es) a reader cannot fetch; a document naming one reads as checked"
exit 1
