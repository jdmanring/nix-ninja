#!/usr/bin/env bash
# ONE QUESTION ANSWERED IN TWO PLACES. This tree's most expensive recurring
# defect is a predicate written twice, and the two copies diverging without
# either one looking wrong. `header_like` listed eleven suffixes in the driver
# while the task binary's placement rule listed four, so a header was declared
# by one crate and placed as a symlink by the other; the fix (ea1abcc) cost a
# GLOBAL RE-KEY because half of it lands inside the task binary's fileset.
#
# NO OTHER INSTRUMENT HERE CAN SEE IT. A semantic index answers "who calls X",
# so it finds sites that share a SYMBOL and is structurally blind to sites
# that share a MEANING through separately written code. Measured on the
# outer-output class: a referencing sweep over `normalize_build_path` reaches
# two of its three sites and cannot reach `add_derived_file`, which resolves
# the same value through `files.lookup` instead.
#
# THE SIGNATURE IS SET OVERLAP, NOT EQUALITY. Two identical lists are a
# duplicate; two lists sharing most members are a duplicate that has ALREADY
# diverged, which is the state that ships a defect. Both are reported, and the
# diverged pair is the one that matters.
#
# A over the working tree, seconds. `--control REV` runs it against a revision
# instead, which is how its own discriminating power is checked: at ea1abcc^
# it must name the header pair, and a run that names nothing there is an
# apparatus result rather than a clean tree.
set -u
# SELF-LOCATING: the tracked copy sits one level below the root and the private
# corpus symlinks it from one level deeper, so a hardcoded depth breaks one of
# the two callers. Walk up to Cargo.toml instead.
ROOT=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
[ -f "$ROOT/Cargo.toml" ] || { echo "NO ANSWER: no Cargo.toml above $0"; exit 2; }
cd "$ROOT" || exit 2

MIN_OVERLAP=${NN_DUP_MIN_OVERLAP:-3}
# THE CONTROL RUNS FIRST, IN THIS RUN, NOT AT A REVISION SOMEONE REMEMBERS.
# This sweep went blind twice while it was being written, once by truncating
# task.rs at a module-scope `#[cfg(test)]` static and once by brace matching
# that ran to end of file, and BOTH TIMES THE WORKING-TREE RUN STILL PRINTED A
# PLAUSIBLE NUMBER AND THE WORD PASS. A sweep that can go blind is only
# trustworthy paired with a case it must find, so ea1abcc^ is rebuilt and
# swept before this tree is, and a run that cannot name the header-suffix pair
# that commit unified refuses instead of reporting.
CONTROL_REV=${NN_DUP_CONTROL_REV:-ea1abcc^}

sweep() {  # sweep <root>
  # `-I` IS LOAD-BEARING. The helper is written to a temp file, so without it the
  # interpreter prepends that world-writable directory to `sys.path` and imports
  # there resolve to whatever else sits beside the script. `-I` implies `-P`
  # (Python 3.11+), which keeps the script's own directory off the path; the
  # script imports nothing but the standard library, so a wider path buys it
  # nothing and costs a code-execution hole in a checkout anyone can write to.
  python3 -I "$PYPROG" "$1" "$MIN_OVERLAP"
}

root=.
tmp=
if [ "${1:-}" = "--control" ]; then
  rev=${2:?--control needs a revision}
  tmp=$(mktemp -d) || exit 2
  trap 'rm -rf "$tmp"' EXIT
  git archive "$rev" crates | tar -x -C "$tmp" || { echo "NO ANSWER: cannot read $rev"; exit 2; }
  root=$tmp
  echo "control $rev"
fi

# A PRIVATE DIRECTORY, NOT A NAMED TEMP FILE: `mktemp` makes it 0700, so the
# helper cannot be replaced or shadowed between being written and being run.
PYDIR=$(mktemp -d) || exit 2
PYPROG=$PYDIR/sweep.py
trap 'rm -rf "$PYDIR"; [ -n "${tmp:-}" ] && rm -rf "$tmp"' EXIT
cat > "$PYPROG" <<'PY'
import os, re, subprocess, sys
from itertools import combinations

root, min_overlap = sys.argv[1], int(sys.argv[2])
LIT = re.compile(r'"((?:[^"\\\\]|\\\\.)*)"')

def interesting(s):
    # A predicate member is a short bare token. Format strings, paths and
    # prose share the literal syntax and are not answers to a question.
    return 0 < len(s) <= 20 and not re.search(r'[\s/{}%\\:=]', s)

clusters = []
# ASSERTED ONCE, BEFORE THE WALK: no tracked Rust lives under a dot-directory,
# so skipping those directories cannot hide source. This is the ratchet, not the
# justification. REPRODUCED 2026-11-16 by audit: `os.path.isdir('.git')` is FALSE
# in a linked worktree, where `.git` is a FILE, and this tree holds seven of those
# - so the check was skipped in exactly the configuration that caused the bug it
# guards. That half is fixed by asking git. THE OTHER HALF IS A `raise`, NOT AN
# `assert`: `assert` is compiled out by `python3 -O` and PYTHONOPTIMIZE, where
# the ratchet then went silent and the sweep reported 15922 pairs over seven
# worktrees' source with no cause named. Reading git's STATUS also distinguishes
# "nothing tracked there" from "git could not answer".
try:
    tracked = subprocess.run(['git', 'ls-files', '*.rs'], capture_output=True,
                             text=True, check=True).stdout.split()
except (OSError, subprocess.CalledProcessError):
    tracked = None
if tracked is not None:
    bad = [p for p in tracked if any(s.startswith('.') for s in p.split('/'))]
    if bad:
        print("NO ANSWER: tracked Rust lives under a dot-directory, which the walk skips:",
              file=sys.stderr)
        for p in bad[:5]:
            print("  " + p, file=sys.stderr)
        sys.exit(2)
for dirpath, dirnames, filenames in os.walk(root):
    # A DOT-DIRECTORY IS NOT THIS TREE'S SOURCE. `.claude/worktrees/` holds one
    # full checkout per agent, so a session that had spawned SEVEN of them made
    # the sweep read 187 source files from those copies against this tree's 29,
    # and report 15922 pairs where a pristine clone reports 421. The verdict was
    # not wrong about the pairs it found, it was about a DIFFERENT POPULATION,
    # which is the shape this project records for a gate whose tree is not the
    # tree claimed.
    dirnames[:] = [d for d in dirnames
                   if d not in ('target', 'vendor-n2') and not d.startswith('.')]
    for fn in filenames:
        if not fn.endswith('.rs'):
            continue
        path = os.path.join(dirpath, fn)
        if '/tests/' in path or '/benches/' in path:
            continue
        try:
            lines = open(path, encoding='utf-8', errors='replace').read().splitlines()
        except OSError:
            continue
        # A FIXTURE IS NOT A SECOND ANSWER. Test code names the same tokens on
        # purpose, and a test asserting a predicate's list is the thing that
        # keeps the list honest rather than a duplicate of it.
        # NO `#[cfg(test)]` TRUNCATION. Two attempts at it both failed
        # QUIET and in the reassuring direction: cutting at the first
        # attribute discarded most of task.rs, which gates a module-scope
        # static on it, and line-based brace matching runs to end of file
        # whenever a test module holds an unbalanced brace inside a string or
        # a comment. Either way the sweep reports PASS over a fraction of the
        # tree. In-tree test modules are left in and their pairs are accepted
        # by name below, which is noisy and cannot go blind.
        lines = list(enumerate(lines, 1))
        hits = []
        for n, line in lines:
            if line.lstrip().startswith('//'):
                continue
            for m in LIT.finditer(line):
                if interesting(m.group(1)):
                    hits.append((n, m.group(1)))
        # A cluster is a run of literals with no more than two lines between
        # them: `matches!(x, "a" | "b")` on one line and a `||` chain one per
        # line are both this shape.
        cur = []
        for n, s in hits:
            if cur and n - cur[-1][0] > 2:
                if len({t for _, t in cur}) >= min_overlap:
                    clusters.append((os.path.relpath(path, root), dict(
                        (t, ln) for ln, t in reversed(cur))))
                cur = []
            cur.append((n, s))
        if cur and len({t for _, t in cur}) >= min_overlap:
            clusters.append((os.path.relpath(path, root), dict(
                (t, ln) for ln, t in reversed(cur))))

# PAIRS READ AND ACCEPTED. Each entry is the two files and the members they
# share, never a line number, so it survives the code moving and stops
# matching the moment either list changes. A pair is added here only after
# both sites have been opened and the reason written.
ACCEPTED = {
    (('crates/deps-infer/src/gcc_include_parser.rs',
      'crates/nix-ninja-task/src/main.rs'),
     frozenset({'-iquote', '-isystem', '-idirafter'})):
        "-I is parsed separately because it has three spellings (-Idir, -I dir, "
        "-I=dir) where the other three have two",
    (('crates/deps-infer/src/gcc_include_parser.rs',
      'crates/nix-ninja/src/task.rs'),
     frozenset({'-iquote', '-isystem', '-idirafter'})):
        "same, against the driver's two readers of the same flags",
    (('crates/deps-infer/src/gcc_depfile_parser.rs',
      'crates/nix-ninja/src/task.rs'),
     frozenset({'gcc', 'g++', 'clang', 'clang++', 'cc', 'c++'})):
        "different questions: SUPPORTED_COMPILERS asks what can be run to "
        "produce a depfile, command_names_toolchain asks what needs the cc "
        "wrapper as an input. The emcc/em++ gap is covered by task_needs_cc's "
        "output-shape arm (3ec8201)",
}

pairs = []
accepted = 0
for (fa, ma), (fb, mb) in combinations(clusters, 2):
    sa, sb = set(ma), set(mb)
    la = min(ma.values())
    lb = min(mb.values())
    if (fa, la) == (fb, lb):
        continue
    shared = sa & sb
    if len(shared) < min_overlap:
        continue
    if ACCEPTED.get((tuple(sorted((fa, fb))), frozenset(shared))) is not None:
        accepted += 1
        continue
    pairs.append((len(shared), sa == sb,
                  fa, min(ma[t] for t in shared), sa,
                  fb, min(mb[t] for t in shared), sb))

pairs.sort(key=lambda p: (-p[0], p[2]))
print(f"literal groups {len(clusters)}   unaccepted pairs {len(pairs)}   accepted {accepted}   min overlap {min_overlap}")
if not pairs:
    print("PASS: no question is answered by two separate literal sets")
    sys.exit(0)
for n, same, fa, la, sa, fb, lb, sb in pairs:
    kind = "DUPLICATE" if same else "DIVERGED"
    print(f"\n{kind}: {n} shared members")
    print(f"  {fa}:{la}  {sorted(sa)}")
    print(f"  {fb}:{lb}  {sorted(sb)}")
    if not same:
        only_a, only_b = sorted(sa - sb), sorted(sb - sa)
        print(f"  only in the first: {only_a}")
        print(f"  only in the second: {only_b}")
print(f"\nFAIL: {len(pairs)} pair(s); export one predicate or state why two are correct")
sys.exit(1)
PY

if [ -z "$tmp" ]; then
  ctl=$(mktemp -d) || exit 2
  if ! git archive "$CONTROL_REV" crates 2>/dev/null | tar -x -C "$ctl"; then
    rm -rf "$ctl"; echo "NO ANSWER: cannot read the control $CONTROL_REV"; exit 2
  fi
  ctl_out=$(sweep "$ctl" 2>&1); rm -rf "$ctl"
  # The control's own signature: the eleven-suffix driver predicate against
  # the four-suffix placement rule.
  if ! printf '%s' "$ctl_out" | grep -q "'hxx'"; then
    # THE CAUSE IS NAMED WHEN THE SWEEP ITSELF RAISED ONE, because the generic
    # headline sent the operator to the wrong file. REPRODUCED 2026-11-16 by
    # audit: the dot-directory ratchet firing printed "this sweep is blind" over
    # a traceback, when the sweep could see perfectly well and the tree had a
    # defect the ratchet exists to report.
    if printf '%s' "$ctl_out" | grep -q 'tracked Rust lives under a dot-directory'; then
      echo "NO ANSWER: the control cannot be judged while the ratchet is failing"
      printf '%s\n' "$ctl_out" | grep 'tracked Rust lives under a dot-directory'
      exit 2
    fi
    echo "NO ANSWER: the control $CONTROL_REV does not name the header pair,"
    echo "so this sweep is blind and its verdict on this tree means nothing."
    printf '%s\n' "$ctl_out" | head -3
    exit 2
  fi
  echo "control $CONTROL_REV names the header pair; the sweep can see"
fi

sweep "$root"
