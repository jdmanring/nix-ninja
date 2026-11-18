#!/usr/bin/env bash
# DOES EVERY PUBLISHED GATE FIND THIS REPOSITORY?
#
# This is the defect publishing itself introduced. Most gates under `gates/`
# used to locate the repository as `dirname $0` plus two levels, which was
# correct in the private corpus (`local/gates/`) and wrong here: from `gates/`
# that path is the PARENT of the repository. Every such gate died in a clean
# clone with a nix error naming no flake, so the corpus was readable and not
# runnable, and the same files worked in the tree that wrote them.
#
# WHY A STATIC CHECK RATHER THAN RUNNING THEM. Running the corpus is minutes to
# hours and needs a daemon. The defect is a property of the file text, and the
# spellings are few, so this asks the question in seconds on a bare clone. The
# companion evidence is a sweep that ran all 81 in a clone, which is where the
# two the first version of this missed were found.
#
# THE PATTERN IS `.` `.` REACHED THROUGH ANY NAMED DIRECTORY, not only through
# `dirname $0`. Measured: two gates hoisted the directory into
# `$HERE=$(cd "$(dirname "$0")" && pwd)` and then wrote `$HERE/../..`, which a
# `dirname`-only pattern does not see.
#
#   PASS       every gate resolves the repository by walking to Cargo.toml
#   FAIL       a gate climbs a fixed depth
#   NO ANSWER  there is no gates/ directory to read, or it holds no scripts
set -uo pipefail
ROOT=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
[ -f "$ROOT/Cargo.toml" ] || { echo "NO ANSWER: no Cargo.toml above $0"; exit 2; }
cd "$ROOT" || exit 2

[ -d gates ] || { echo "NO ANSWER: no gates/ directory under $ROOT"; exit 2; }

PATTERN='(\$[A-Za-z_][A-Za-z0-9_]*|dirname[^)]*)\)?/\.\./\.\.|cd +\.\./\.\.'
seen=0; bad=0
for f in gates/*.sh; do
  [ -f "$f" ] || continue
  seen=$((seen + 1))
  # COMMENT LINES ARE NOT CODE, and this gate's own header spells the pattern
  # out to explain it, so a whole-file match reports itself. The same reasoning
  # applies to every gate: a header describing the defect is documentation.
  hits=$(command grep -vE '^[[:space:]]*#' "$f" | command grep -nE "$PATTERN")
  if [ -n "$hits" ]; then
    echo "FAIL fixed-depth lookup   $(basename "$f")"
    printf '%s\n' "$hits" | sed 's/^/    /' | head -2
    bad=$((bad + 1))
  fi
done

[ "$seen" -gt 0 ] || { echo "NO ANSWER: read 0 scripts under gates/"; exit 2; }
if [ "$bad" -gt 0 ]; then
  echo
  echo "FAIL: $bad of $seen published gate(s) climb a fixed depth, so they"
  echo "  cannot run in a clone. Walk up to the directory holding Cargo.toml."
  exit 1
fi
echo
echo "PASS: every published gate locates the repository from any depth ($seen read)."
exit 0
