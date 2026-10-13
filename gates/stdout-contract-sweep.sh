#!/usr/bin/env bash
# EVERY STDOUT WRITE IN BOTH BINARIES, CLASSIFIED AGAINST THE LOGGING CONTRACT.
#
# The contract: stdout is DATA (`--version`, `-t list`, `-t drv` and `-t compdb`
# JSON, and a `-v` build's relayed task output). Everything else is stderr
# carrying a `nix-ninja:` / `nix-ninja-task:` / `nix-ninja-dynamic-task:`
# prefix. So the prefix IS the discriminator: a format string that opens with
# one is a diagnostic by its own declaration, and a diagnostic on stdout is
# the defect. Nothing here has to guess intent.
#
# WHY A STATIC ARM WHEN logging-contract-repro.sh ALREADY EXISTS. That gate is
# dynamic and correct, and it can only see the call sites its fixture reaches:
# it drives one build and one `-t drv`, so a diagnostic on a path no fixture
# takes is invisible to it. Measured here rather than argued: it is green while
# thirty stdout diagnostics stand, twenty-six of them behind `syncqt` and
# `-privateIncludeDir` conditions no fixture in this corpus meets. The two arms
# answer different questions and neither replaces the other.
#
# THIS IS A RATCHET, NOT A REPAIR, and the distinction is the fileset. Twenty-
# six of the thirty are in `nix-ninja-task`, which is inside the task binary's
# allowlist, so moving them re-keys every banked compile in the consumer's
# store. They are also HARMLESS TODAY, which is why they have survived: a
# builder's stdout and stderr both land in the derivation log, and the `-v`
# transcript the driver reads is written from the CHILD's pipes
# (`spawn_process_teed` writes `log_path` from the merged pipe, not from this
# process's stdout), so no declared output carries them and CMake's ABI parse
# is not exposed. The hazard is latent rather than live. So this arm holds the
# count where it is and fails on GROWTH, which costs nothing, instead of going
# red for a class whose repair is owed to a batch.
#
# Configuration-independent: it reads source and builds nothing. Seconds.
#   PASS       no crate exceeds its baseline, and both self-controls fire
#   FAIL       a new stdout diagnostic, or a control that did not discriminate
set -u
# SELF-LOCATING, not depth-dependent. This gate is invoked from the tracked
# `gates/` directory and symlinked from the private corpus beside the other
# arms, which sit one level deeper. Hardcoding either depth breaks the other
# caller, so the repository root is found by walking up to the directory that
# holds Cargo.toml, and the gate works wherever it is reached from.
ROOT=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
[ -f "$ROOT/Cargo.toml" ] || { echo "NO ANSWER: no Cargo.toml above $0"; exit 2; }
cd "$ROOT" || exit 2
DRIVER_BASELINE=4
TASK_BASELINE=26

# THE DETECTOR. A `println!` and its format string are usually on separate
# lines, so the macro is carried forward until a literal or a statement end
# arrives; an `eprintln!` on the way cancels it, which is what stops the
# adjacent-macro false positive that the first version of this had.
scan() {
  awk -v FILE="$1" '
    /^[ \t]*\/\// { next }
    {
      if ($0 ~ /println!/ && $0 !~ /eprintln!/) { pend = 1; pline = FNR }
      else if (pend && $0 ~ /eprintln!/)        { pend = 0 }
      if (pend && match($0, /"nix-ninja(-task|-dynamic-task)?: /)) {
        printf "%s:%d  %s\n", FILE, pline, substr($0, RSTART, 58); pend = 0
      }
      if ($0 ~ /;[ \t]*$/) pend = 0
    }' "$1"
}
sweep() { # sweep <dir>
  local f
  for f in $(command grep -rl 'println!' "$1" 2>/dev/null); do scan "$f"; done
}

T=$(mktemp -d -t nn-stdoutsweep-XXXXXX); trap 'rm -rf "$T"' EXIT

# CONTROL ONE, POSITIVE: a newly added stdout diagnostic must be caught. It is
# written into a file the sweep actually walks, in the same shape the real
# sites have (macro and literal on separate lines), because a control the
# subject never reads proves the harness and not the sweep.
mkdir -p "$T/src"
printf 'fn a() {\n    println!(\n        "nix-ninja: a newly added diagnostic {}",\n        1\n    );\n}\n' > "$T/src/added.rs"
# CONTROL TWO, NEGATIVE: the same text on stderr, and a legitimate DATA site,
# must both stay unflagged. Without this the sweep could pass by flagging
# everything.
printf 'fn b() {\n    eprintln!(\n        "nix-ninja: a correctly placed diagnostic {}",\n        1\n    );\n    println!("1.10.0");\n    println!("{}", to_json(&entries)?);\n}\n' > "$T/src/ok.rs"

caught=$(sweep "$T/src" | command grep -c 'added.rs' || true)
leaked=$(sweep "$T/src" | command grep -c 'ok.rs' || true)
if [ "$caught" -ne 1 ]; then
  echo "NO ANSWER: the positive control was not caught ($caught), so this sweep is inert"; exit 2
fi
if [ "$leaked" -ne 0 ]; then
  echo "FAIL: the negative control was flagged ($leaked); the sweep does not discriminate"
  sweep "$T/src" | command grep 'ok.rs' | sed 's/^/    /'
  exit 1
fi

sweep crates/nix-ninja/src      > "$T/driver.txt"
sweep crates/nix-ninja-task/src > "$T/task.txt"
d=$(wc -l < "$T/driver.txt"); t=$(wc -l < "$T/task.txt")
echo "controls    positive caught, negative and two data sites clean"
echo "driver      $d stdout diagnostic(s), baseline $DRIVER_BASELINE"
echo "task        $t stdout diagnostic(s), baseline $TASK_BASELINE"

rc=0
if [ "$d" -gt "$DRIVER_BASELINE" ] || [ "$t" -gt "$TASK_BASELINE" ]; then
  echo "FAIL: a stdout diagnostic was added. Put it on stderr, or move the"
  echo "  baseline here in the same commit and say in the message why."
  cat "$T/driver.txt" "$T/task.txt" | sed 's/^/    /'
  rc=1
elif [ "$d" -lt "$DRIVER_BASELINE" ] || [ "$t" -lt "$TASK_BASELINE" ]; then
  # A DROP IS NOT A PASS EITHER. The baseline is the ledger for an owed batch
  # repair, and leaving it above the truth would silently re-open room to grow.
  echo "FAIL: the count DROPPED, so the repair landed and this baseline is stale."
  echo "  Lower it to $d and $t in the same commit as the repair."
  rc=1
else
  echo "PASS: no stdout diagnostic added; the owed repair is $((d + t)) sites, unchanged"
fi
echo
echo "THE REPAIR IS OWED TO A BATCH AND NOT TO THIS ARM. $t of them are in"
echo "nix-ninja-task, inside the task binary's fileset, so moving them re-keys"
echo "every banked compile. They are latent rather than live: a builder's"
echo "stdout reaches the derivation log, and the -v transcript is written from"
echo "the child's pipes, so no declared output carries them."
exit $rc
