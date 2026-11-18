#!/usr/bin/env bash
# DOES THE PUBLIC STATUS PAGE STILL DESCRIBE THIS TREE?
#
# docs/STATUS.md is the one page a stranger reads first, and every sentence on
# it is a claim about this repository that ROTS IN SILENCE: a link moves, a
# version advances in Cargo.toml while the page keeps the old number, a gate the
# page names gets renamed. None of those change the page's text, so nothing
# fails and the page goes on reading as current. That is the same shape as a
# stale commit count, one level up: a document asserting a state nobody derives.
#
# THE PAGE IS ALSO THE LAST PUBLIC ARTIFACT WITH NO GATE AT ALL (measured
# 2026-11-17: `grep -l STATUS.md gates/*.sh local/gates/*.sh` returned nothing).
# Every other public document already had one: the roadmap has
# roadmap-numbers.sh and the changelog has changelog-citations.sh. The status
# page had a reader's trust.
#
# WHAT IT CHECKS, derived from the page's own falsifiable claims:
#   links       every relative link resolves to a tracked file
#   version     the bolded version equals Cargo.toml's, and the milestone
#               it names is the one the roadmap has open
#   gates       every gates/<name>.sh the page names exists
#   tag         a referenced vX.Y.Z tag exists in this clone
#
# A COUNT IS NOT DERIVED HERE: the page deliberately restates no number that
# roadmap-numbers.sh already owns, and duplicating one would create the second
# live record this corpus exists to prevent.
#
# NO BUILD, NO DAEMON, NO MACHINE: seconds, safe during a round. It reads the
# DOCUMENT, so a red result means fix the page, not do-not-spend-the-machine.
#
#   PASS       every link resolves and every derived value matches
#   FAIL       a claim on the page disagrees with the tree
#   NO ANSWER  the page, Cargo.toml or the roadmap is absent (a public clone
#              without the roadmap is not this gate's subject)
set -uo pipefail
# SELF-LOCATING, not depth-dependent, and it must follow the symlink: this gate
# is invoked from the tracked `gates/` directory and symlinked into the private
# corpus, which sits one level deeper. Walking up to the directory that holds
# Cargo.toml works from either, which is the pattern the other published gates
# already use.
ROOT=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
[ -f "$ROOT/Cargo.toml" ] || { echo "NO ANSWER: no Cargo.toml above $0"; exit 2; }
cd "$ROOT" || exit 2

PAGE=docs/STATUS.md
[ -f "$PAGE" ] || { echo "NO ANSWER: $PAGE is absent"; exit 2; }
[ -f Cargo.toml ] || { echo "NO ANSWER: Cargo.toml is absent, so the version cannot be derived"; exit 2; }

fail=0; checked=0
bad() { printf 'FAIL %-22s %s\n' "$1" "$2"; fail=$((fail + 1)); }
ok()  { printf 'OK   %-22s %s\n' "$1" "$2"; checked=$((checked + 1)); }

# A MENTION IS NOT A LINK, and a relative one resolves against docs/, not the
# root. An external URL is not this gate's subject: it can rot for reasons no
# commit here fixes, so it is counted and reported rather than failed.
links=$(command grep -oE '\]\([^)]+\)' "$PAGE" | sed -E 's/^\]\(//; s/\)$//')
rel=0; ext=0; badlinks=0
while IFS= read -r l; do
  [ -n "$l" ] || continue
  case "$l" in
    http://*|https://*) ext=$((ext + 1)); continue ;;
  esac
  rel=$((rel + 1))
  if [ ! -e "docs/$l" ]; then
    bad "link resolves" "$l (from $PAGE)"
    badlinks=$((badlinks + 1))
  fi
done <<LINKS
$links
LINKS
[ "$rel" -gt 0 ] || bad "link population" "parsed no relative link from $PAGE, so the check read nothing"
[ "$badlinks" -eq 0 ] && [ "$rel" -gt 0 ] && ok "links" "$rel relative link(s) resolve, $ext external not judged"

# THE VERSION IS STATED TWICE ON THE PAGE, bolded in prose and again in the
# Authority section, so both spellings are read: the count is the discriminator
# for a page that grows a third, as the changelog table's row count was.
cargo=$(command grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')
[ -n "$cargo" ] || { echo "NO ANSWER: Cargo.toml names no version"; exit 2; }
claimed=$(command grep -oE '\*\*[0-9]+\.[0-9]+\.[0-9]+\*\*' "$PAGE" | tr -d '*' | sort -u)
if [ -z "$claimed" ]; then
  bad "version" "the page states no bolded development version"
elif [ "$claimed" != "$cargo" ]; then
  bad "version" "the page says $claimed, Cargo.toml says $cargo"
else
  ok "version" "$claimed matches Cargo.toml"
fi

# A TAG THE PAGE NAMES MUST EXIST. The page's tag sentence is the one claim a
# reader acts on directly, by checking it out.
#
# A MISSING TAG IS NOT A STALE PAGE UNTIL THIS CLONE PROVES IT FETCHED TAGS.
# `git clone --depth 1` fetches no tags at all, so the first version of this
# check reported the page stale in the very clone CI uses. The clone's
# SHALLOWNESS is the wrong question: a full clone made with `--no-tags` is not
# shallow and is just as tag-less. What decides it is whether the repository has
# any tag at all, so a tree with none is refused rather than judged.
tag=$(command grep -oE '\bv[0-9]+\.[0-9]+\.[0-9]+\b' "$PAGE" | head -1)
if [ -n "$tag" ]; then
  anytag=$(git tag -l | head -1)
  if [ -z "$anytag" ]; then
    echo "NO ANSWER: $PAGE names $tag, but this working copy carries no tags at all."
    echo "  A depth-1 or --no-tags clone is the usual cause, and so is a fresh"
    echo "  'git clone', which fetches none by default. This is not a stale page:"
    echo "  the check cannot see a tag, so it refuses rather than blame the text."
    echo "  Fetch them and re-run:  git fetch --tags"
    exit 2
  fi
  if git rev-parse --verify --quiet "refs/tags/$tag" >/dev/null 2>&1; then
    ok "tag exists" "$tag"
  else
    bad "tag exists" "$tag is named on the page and is not a tag in this clone, which carries $(git tag -l | wc -l) tag(s)"
  fi
fi

# EVERY GATE THE PAGE NAMES IS IN THE PUBLIC TREE. The page's verification
# surface is a promise about what a clone can run, so a named script that has
# moved is a broken promise the page keeps making.
named=$(command grep -oE 'gates/[A-Za-z0-9._-]+\.sh' "$PAGE" | sort -u)
if [ -z "$named" ]; then
  bad "gate population" "the page names no gates/<script>, so the check read nothing"
else
  g=0; badnamed=0
  while IFS= read -r s; do
    [ -n "$s" ] || continue
    g=$((g + 1))
    [ -f "$s" ] || { bad "gate exists" "$s is named on the page and is not in this tree"; badnamed=$((badnamed + 1)); }
  done <<GATES
$named
GATES
  # NOT gated on $fail: that counts every earlier check, so a broken link would
  # silently suppress this line and the "gates named" reading would go missing
  # rather than report. Each check carries its own count.
  [ "$badnamed" -eq 0 ] && ok "gates named" "$g script(s) exist"
fi

# A COUNT THE PAGE STATES ABOUT ITS OWN CORPUS. `gates/` holds many scripts and
# the page says how many; that number rots the moment one is added, and nothing
# derived it. The page's phrasing is the population: the count it states IS the
# number of `gates/*.sh` in the tree.
stated=$(command grep -oE '`gates/` carries [0-9]+ scripts' "$PAGE" | command grep -oE '[0-9]+' | head -1)
if [ -n "$stated" ]; then
  actual=$(command ls gates/*.sh 2>/dev/null | wc -l | tr -d ' ')
  if [ "$stated" = "$actual" ]; then
    ok "corpus size" "$stated script(s) under gates/, as stated"
  else
    bad "corpus size" "the page says $stated scripts under gates/, the tree has $actual"
  fi
fi

echo
if [ "$fail" -eq 0 ]; then
  echo "PASS: every claim on $PAGE agrees with the tree ($checked checked)."
  exit 0
fi
echo "FAIL: $fail claim(s) on $PAGE no longer describe this tree."
exit 1
