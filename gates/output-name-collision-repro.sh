#!/usr/bin/env bash
# ONE EDGE, TWO RUNS, ONE OUTPUT NAME: the collapse's emission surface.
#
# THE CLASS, measured 2026-10-02 off the consumer's first server round. The
# daemon refused registrations with
#
#   Trying to register a realisation of '<drv>^<output>', but we already have
#   another one locally
#
# and the shape is NEITHER same-drv nor cross-drv as both a peer and this tree
# first framed it: ONE EDGE, MANY DRVS, ONE OUTPUT NAME. `normalize_output` is
# a lossy label (`.libs/version.o` -> `-.libs-version.o`) and
# `Placeholder::standard_output` hashes the NAME ALONE, so the emitted name is
# the collision surface. The guard at `task.rs:4200` refuses a collision WITHIN
# one edge and does not fire here, because each drv declares ONE output.
#
# WHAT THIS GATE ASSERTS. Two runs of ONE edge whose discovered input set
# differs emit two DIFFERENT task derivations that declare the SAME output
# name. That is sufficient for the daemon to refuse the second registration,
# and it is the surface a fix must move.
#
# RED BY DESIGN UNTIL THE NAME IS MADE UNIQUE PER BUILD PATH. Making it unique
# is emission-side (the name feeds `drv.outputs` and `NIX_NINJA_OUTPUTS`), so
# it is a GLOBAL RE-KEY and does not ride a free bump. When that change lands,
# the two runs must emit DIFFERENT names and this gate flips: the assertion
# below becomes `names differ`, which is why the two arms are printed by the
# same code path.
#
# THE CONTROL IS MANDATORY AND IT IS THE SECOND EDGE. A pair of edges whose
# outputs do NOT normalize alike must emit two names, so an arm that reports
# "one name" can only be reporting the collision and not a fixture that
# emitted one edge twice. Without it this gate passes on a fixture that never
# built the second edge.
#
#   NN_PIN_REV   the pin, for a control arm that must still carry the collision
#   NN_GATE_SRC  driver/task from that revision (default: this tree)
#   KEEP=1       keep the fixture
#
# SECONDS AND NO DAEMON: `-t drv` RESOLVES, it does not build. Safe on a
# loaded box.
set -uo pipefail
# NEVER GLOB /nix/store: zsh or bash expanding `/nix/store/*...` reads and
# sorts every entry, and over 650k entries one expansion reached 77 GB RSS
# and stalled a live round (2026-10-17). Stream it and stop at the first hit.
store_find() {  # store_find NAME_PATTERN [SUBPATH]
  find /nix/store -maxdepth 1 -name "$1" ! -name '*.drv' -print 2>/dev/null |
    while IFS= read -r d; do
      [ -e "$d${2:-}" ] && { printf '%s\n' "$d${2:-}"; break; }
    done
}

F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")/../.." && pwd)
SRC=${NN_GATE_SRC:-$REPO}

b() { nice -n 10 nix build --extra-experimental-features "$F" \
        --no-link --print-out-paths "$@" 2>/dev/null; }

NN=$(b "$SRC#nix-ninja")      || { echo "NO ANSWER: the driver did not build"; exit 2; }
TASK=$(b "$SRC#nix-ninja-task") || { echo "NO ANSWER: the task binary did not build"; exit 2; }

CU=$(store_find '*coreutils-9*' /bin)
BASHB=$(store_find '*bash-5.3' /bin)
PE=$(store_find '*patchelf*' /bin)
CCW=$(store_find '*gcc-wrapper-16*' /bin)
GR=$(store_find '*gnugrep-3*' /bin)
NIXB=$(store_find '*nix-2*' /bin)
for t in "$CU" "$BASHB" "$PE" "$CCW" "$GR" "$NIXB"; do
  [ -n "$t" ] || { echo "NO ANSWER: a store tool did not resolve"; exit 2; }
done

D=$(mktemp -d -t nn-oname-XXXXXX); S=$D/build; mkdir -p "$S" "$D/symbols"
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D"' EXIT
printf 'x\n' > "$D/symbols/a"

# `run <ninja-file> <target> > json`. The blanket is OFF so the input set has
# ONE route: the edge, which is what makes run two's drv differ from run one's.
run() { # run <tree> <target>
  ( cd "$S"
    PATH="$1/bin:$TASK/bin:$NIXB:$CCW:$CU:$BASHB:$PE:$GR" \
    NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
    NIX_CONFIG="experimental-features = $F" \
    NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
    "$1/bin/nix-ninja" -t drv "$2" 2>/dev/null
  )
}

# The name a drv declares, and its path: the two facts the class is about.
names() { python3 - "$1" <<'PY'
import json, sys
try:
    d = json.load(open(sys.argv[1]))
except Exception:
    print("PARSE-FAIL"); sys.exit(0)
for k, v in d.items():
    if isinstance(v, dict) and "outputs" in v:
        print("%s %s" % (k, ",".join(sorted(v["outputs"].keys()))))
        break
else:
    print("NO-DRV")
PY
}

# --- arm: ONE edge, two input sets -> two drvs, ONE name -------------------
cat > "$S/build.ninja" <<'EOF'
rule cc
  command = sh -c 'echo one > $out'
build .libs/version.o: cc
EOF
run "$NN" .libs/version.o > "$D/c1.json" || { echo "NO ANSWER: run one did not emit"; exit 2; }
cat > "$S/build.ninja" <<'EOF'
rule cc
  command = sh -c 'echo one > $out'
build .libs/version.o: cc | ../symbols
EOF
run "$NN" .libs/version.o > "$D/c2.json" || { echo "NO ANSWER: run two did not emit"; exit 2; }

A=$(names "$D/c1.json"); B=$(names "$D/c2.json")
echo "collide run1  $A"
echo "collide run2  $B"
DA=${A%% *}; NA=${A#* }; DB=${B%% *}; NB=${B#* }

# --- the PIN arm: the class must be in the pin too ------------------------
# Without it a head arm that agrees with a fixture that never reproduced the
# class cannot be told apart, and this tree has shipped that shape before. The
# pin is OPTIONAL only because the gate is usable in a tree with no pin to
# hand; when NN_PIN_REV is set the arm RUNS and a pin that does not carry the
# collision FAILS the gate rather than passing quietly.
PIN_REV=${NN_PIN_REV:-}
if [ -n "$PIN_REV" ]; then
  PNN=$(b "git+file://$REPO?rev=$PIN_REV#nix-ninja") \
    || { echo "NO ANSWER: the pin driver did not build"; exit 2; }
  cat > "$S/build.ninja" <<'EOF'
rule cc
  command = sh -c 'echo one > $out'
build .libs/version.o: cc
EOF
  run "$PNN" .libs/version.o > "$D/p1.json" || { echo "NO ANSWER: pin run one did not emit"; exit 2; }
  cat > "$S/build.ninja" <<'EOF'
rule cc
  command = sh -c 'echo one > $out'
build .libs/version.o: cc | ../symbols
EOF
  run "$PNN" .libs/version.o > "$D/p2.json" || { echo "NO ANSWER: pin run two did not emit"; exit 2; }
  P1=$(names "$D/p1.json"); P2=$(names "$D/p2.json")
  echo "pin     run1  $P1"
  echo "pin     run2  $P2"
  PA=${P1#* }; PB=${P2#* }
  PD1=${P1%% *}; PD2=${P2%% *}
  case "$P1 $P2" in
    *PARSE-FAIL*|*NO-DRV*|" "*)
      echo "NO ANSWER(control): the pin arm did not emit two readable derivations"; exit 2 ;;
  esac
  if [ "$PA" != "$PB" ] || [ "$PD1" = "$PD2" ]; then
    echo "FAIL(control): the pin does not carry the collision (names $PA / $PB,"
    echo "  drvs $PD1 / $PD2), so this fixture does not reproduce the class and"
    echo "  the head arm proves nothing"
    exit 1
  fi
fi

# --- control: two edges that do NOT normalize alike -> two names ----------
cat > "$S/build.ninja" <<'EOF'
rule cc
  command = sh -c 'echo one > $out'
build alpha.o: cc
build beta.o: cc
EOF
run "$NN" alpha.o > "$D/k1.json" || { echo "NO ANSWER: control run one did not emit"; exit 2; }
run "$NN" beta.o  > "$D/k2.json" || { echo "NO ANSWER: control run two did not emit"; exit 2; }
K1=$(names "$D/k1.json"); K2=$(names "$D/k2.json")
echo "control run1  $K1"
echo "control run2  $K2"
NK1=${K1#* }; NK2=${K2#* }
case "$NK1 $NK2" in
  *PARSE-FAIL*|*NO-DRV*|"")
    echo "NO ANSWER: the control did not emit two readable derivations"; exit 2 ;;
esac
if [ "$NK1" = "$NK2" ]; then
  echo "NO ANSWER(control): two distinct outputs emitted ONE name ($NK1), so the"
  echo "  arm cannot distinguish the collision from a fixture that emits one edge"
  exit 2
fi

case "$A $B" in
  *PARSE-FAIL*|*NO-DRV*|" "*)
    echo "NO ANSWER: an arm did not emit a readable derivation"; exit 2 ;;
esac

# THE ASSERTION, and it is deliberately the RED-BY-DESIGN direction: today the
# two runs DO emit one name, and that is the defect. The gate fails while the
# name is path-lossy and passes when a fix makes it unique.
if [ "$NA" = "$NB" ] && [ "$DA" != "$DB" ]; then
  echo "FAIL: one edge emitted two derivations under ONE output name '$NA'"
  echo "  run1 drv=$DA"
  echo "  run2 drv=$DB"
  echo "  the daemon refuses the second registration of '$NA'; a fix must make the"
  echo "  name unique per build path (emission-side, global re-key)"
  exit 1
fi
if [ "$NA" != "$NB" ]; then
  echo "PASS: the two runs emit DIFFERENT output names ($NA vs $NB), so the name"
  echo "  no longer collides across derivations of one edge"
  exit 0
fi
echo "NO ANSWER: the two runs emitted the same drv ($DA) and the same name ($NA);"
echo "  the differing input set did not re-key, so this fixture proved nothing"
exit 2
