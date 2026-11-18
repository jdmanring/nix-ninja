#!/usr/bin/env bash
# A DIRECTORY ARGUMENT OVER THE UPLOAD CAP, HOLDING NO PYTHON PACKAGE.
#
# gobject-introspection's class, witnessed by the consumer: a generator names
# `../tests/scanner` as a directory argument. Over the cap the driver falls
# back to uploading the directory's importable python packages, which is the
# convention for a sys.path root handed to a generator (chromium's
# inspector_protocol passes all of third_party that way). A DATA tree is not
# that. The fallback found no package, uploaded nothing, printed
#
#   dir arg <path> exceeds 1024 files; uploaded only its python packages (0 files)
#
# and RETURNED SUCCESS, so the task ran with the directory empty. The consumer
# measured what followed: the generator's 18 dependencies failed and 110
# top-level derivations went with them.
#
# TWO ARMS, because the fix has two halves and only one of them is the carry.
#   arm 1  a data directory of gobject-introspection's size is CARRIED, and
#          the generator sees the files. Measured on 1.86.0: tests/scanner is
#          2,216 files and 9.5 MiB, so the fixture's 1,200 sits in the same
#          band, over the package cap of 1024 and under the data cap (`DATA_DIR_CAP`).
#   arm 2  a directory past the data cap is REFUSED, naming it. Every input
#          is a store path in the task's closure, and closure size is daemon
#          lock time here, so the bound stays and says so when it bites.
#
# ARM 1 ASSERTS THE COUNT THE GENERATOR READ, not merely that the build
# succeeded: an edge running against an empty directory still writes an
# output, and a probe that only looks for the file is green either way.
#
# The fixture is over the cap by construction (1200 files, cap 1024) and holds
# no `__init__.py` anywhere, which is the discriminator: add one to any
# subdirectory and the old path applies again, which the control below uses.
#
# Configuration A, a minute (the fixture writes 1200 files).
#   PASS  the carried directory reaches the generator with every file, and
#         the oversized one is refused by name
#   FAIL  the generator sees a different count, or the oversized directory is
#         carried, or either arm fails for another reason
#
# CONTROL, MEASURED at 2ca6d02 (`NN_PROBE_REV=2ca6d02`): the driver proceeds,
# the task is built, and the command dies inside the sandbox with
#
#   FileNotFoundError: [Errno 2] No such file or directory: '../data/scanner'
#
# which is the consumer's shape, a task running against a directory that is
# not there. So the control does NOT exit zero: the driver's silent success
# is spent one layer down, where the failure names a path and no derivation.
# That is the reading the refusal replaces.
set -uo pipefail
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")/../.." && pwd)
[ -f "$REPO/crates/nix-ninja-task/src/main.rs" ] || { echo "NO ANSWER: $REPO is not the nix-ninja tree"; exit 2; }
b(){ nice -n 10 nix build --extra-experimental-features "$F" --no-link --print-out-paths "$@" 2>/dev/null | tail -1; }
if [ -n "${NN_PROBE_REV:-}" ]; then
  W=$(mktemp -d -t nn-dirarg-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$NN_PROBE_REV" || { echo "NO ANSWER: cannot check out $NN_PROBE_REV"; exit 2; }
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  SRC=$W; echo "driver from $NN_PROBE_REV"
else SRC=${NN_GATE_SRC:-$REPO}; fi
NN=$(b "$SRC#nix-ninja");        [ -x "$NN/bin/nix-ninja" ]           || { echo "NO ANSWER: no driver"; exit 2; }
TASK=$(b "$REPO#nix-ninja-task"); [ -x "$TASK/bin/nix-ninja-task" ]   || { echo "NO ANSWER: no task binary"; exit 2; }
px(){ b --impure --expr "let f=builtins.getFlake \"$REPO\"; p=f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1"; }
CU=$(px coreutils); BASH=$(px bash); PY=$(px python3); CC=$(px gcc); PE=$(px patchelf)
NIXP=""
for p in $(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default"); do
  [ -x "$p/bin/nix" ] && { NIXP=$p; break; }
done
GATE_PATH="$NN/bin:$CU/bin:$BASH/bin:$PY/bin:$CC/bin:$NIXP/bin:$TASK/bin:$PE/bin"
for t in nix-ninja sh python3 nix nix-ninja-task patchelf; do
  found=""; IFS=: read -ra dirs <<< "$GATE_PATH"
  for d in "${dirs[@]}"; do [ -x "$d/$t" ] && { found=$d; break; }; done
  [ -n "$found" ] || { echo "NO ANSWER: $t is not on the gate PATH, so this arm would assert nothing"; exit 2; }
done

D=$(mktemp -d -t nn-dirarg-XXXXXX); trap 'chmod -R u+w "$D" 2>/dev/null; rm -rf "$D"' EXIT
mkdir -p "$D/build" "$D/data/annotations" "$D/data/scanner"
# Over the cap by construction, and PROVE it below rather than assume it.
python3 - "$D/data" <<'PY'
import sys, pathlib
root = pathlib.Path(sys.argv[1])
for i in range(1200):
    (root / "scanner" / f"case{i}.h").write_text(f"#define CASE{i} {i}\n")
PY
count=$(find "$D/data" -type f | wc -l)
[ "$count" -gt 1024 ] || { echo "NO ANSWER: fixture holds $count files, not over the 1024 cap; nothing was measured"; exit 2; }
[ -z "$(find "$D/data" -name '__init__.py' -print -quit)" ] || { echo "NO ANSWER: fixture contains a python package, so it is not this class"; exit 2; }

# A heredoc, not printf: the generator's own string holds a newline escape,
# and printf renders it, which splits the python literal across two lines and
# makes the fixture a syntax error instead of a test.
cat > "$D/build/gen.py" <<'GENPY'
import sys, os
d = sys.argv[1]
n = len(os.listdir(os.path.join(d, "scanner")))
open(sys.argv[2], "w").write("/* %d */\n" % n)
GENPY
cat > "$D/build/build.ninja" <<EOF
rule gen
  command = python3 gen.py $D/data \$out
rule gen2
  command = python3 gen.py $D/data2 \$out
build out.h: gen gen.py
build out2.h: gen2 gen.py
EOF
cd "$D/build"
run() {
  env -i HOME="$D" PATH="$GATE_PATH" NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
    NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" NIX_CONFIG="experimental-features = $F" \
    "$NN/bin/nix-ninja" "$2" >"$1" 2>&1
}

# arm 1: the directory is carried, and the generator reads all of it
run "$D/carry.log" out.h; rc=$?
if [ $rc -ne 0 ]; then
  echo "FAIL arm 1: the driver did not carry a $count file data directory (rc=$rc)"
  tail -6 "$D/carry.log" | sed 's/^/  /'
  exit 1
fi
staged=$(find "$D/data/scanner" -type f | wc -l)
got=$(grep -oE '[0-9]+' "$D/build/out.h" 2>/dev/null | head -1)
if [ "$got" != "$staged" ]; then
  echo "FAIL arm 1: the generator counted '${got:-nothing}', not the $staged files staged"
  echo "  an empty directory still lets the edge write an output; the COUNT is the assertion"
  exit 1
fi
echo "  arm 1 OK  carried $count files, and the generator read all $staged"

# arm 2: past the data cap, refuse and name it
#
# A SEPARATE DIRECTORY, because the driver memoises a directory upload by
# path. Growing the one arm 1 used returns arm 1's answer and the arm then
# measures the cache rather than the bound: written that way it reported the
# driver carrying 5,200 files past a limit it had never re-evaluated.
#
# THE CAP IS READ FROM THE SOURCE, because a literal here went stale: the
# cap moved from 4096 to 8192 (6e2ccd3) and the 5,200-file fixture, now under
# it, was carried and read as a failure of the bound for two weeks.
cap=$(command grep -oE '^const DATA_DIR_CAP: usize = [0-9]+;' "$REPO/crates/nix-ninja/src/task.rs" | command grep -oE '[0-9]+')
[ -n "$cap" ] || { echo "NO ANSWER: DATA_DIR_CAP not found in crates/nix-ninja/src/task.rs"; exit 2; }
mkdir -p "$D/data2/scanner"
python3 - "$D/data2" "$((cap + 1000))" <<'PY'
import sys, pathlib
root = pathlib.Path(sys.argv[1])
for i in range(int(sys.argv[2])):
    (root / "scanner" / ("case%d.h" % i)).write_text("#define CASE%d %d\n" % (i, i))
PY
big=$(find "$D/data2" -type f | wc -l)
[ "$big" -gt "$cap" ] || { echo "NO ANSWER: second fixture holds $big files, not over the $cap data cap"; exit 2; }
run "$D/refuse.log" out2.h; rc=$?
if [ $rc -ne 0 ] && grep -q "neither a .*sys.path root nor a data directory" "$D/refuse.log" \
   && grep -qE "dir arg [^ ]*data[0-9]* " "$D/refuse.log"; then
  echo "  arm 2 OK  refused $big files and named the directory"
  echo "PASS: a data directory of the reported size is carried, a runaway one is refused"
  exit 0
fi
if [ $rc -eq 0 ]; then
  echo "FAIL arm 2: the driver carried $big files, past the bound it prints"
  exit 1
fi
echo "FAIL arm 2: failed (rc=$rc) but not with a refusal naming the directory"
tail -6 "$D/refuse.log" | sed 's/^/  /'
exit 1
