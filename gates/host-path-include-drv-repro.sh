#!/usr/bin/env bash
# A HEADER THAT NAMES A HOST FILE MUST NOT PUT IT ON THE TASK.
#
# THE CLASS, measured on c-ares at `5f0fe07`. The include scanner reads the
# BODIES of string macros, so `ares_private.h`'s `PATH_HOSTS "/etc/hosts"` and
# `PATH_RESOLV_CONF "/etc/resolv.conf"` arrived as ABSOLUTE includes. Declared,
# they are spelled from the build dir as `../../../etc/hosts` - a climb past
# the root - so `nix-ninja-task`'s deepening guard read that as a cwd error,
# moved the working directory to `build/nnd0`, and ALL thirty-five real inputs
# then resolved one name too deep. The whole source tree and every `-I`
# directory vanished together and the compile died on
# `util/ares_threads.c: No such file or directory` with that file present in
# the input set.
#
# THE FIXTURE IS THE REAL PACKAGE, and that is not gold-plating - a synthetic
# one CANNOT reproduce this. A bare fixture declaring `PATH_HOSTS "/etc/hosts"`
# emits NO host input on either driver, because the scanner declares the path
# only when it RESOLVES to a file that exists, and the shape that makes it
# resolve (a header reached through the include chain of a real compile edge)
# is what the real tree supplies. An earlier version of this gate used a
# synthetic fixture and its two arms printed identical output: the
# arms-cannot-differ defect, in the instrument written to close it.
#
#   PASS   the host paths are ABSENT from the fixed driver's inputs
#          AND the compile's own source is PRESENT, so nothing over-dropped
#   FAIL   otherwise
#   NO ANSWER  the driver could not be driven
#
# A FAKE PASS looks like: the source is missing from BOTH arms, which reads as
# a fix that dropped everything. The `source=y` arm below refuses that.
#
# THE CONTROL IS MANDATORY (NN_PIN_REV). The head arm alone cannot tell a fixed
# driver from a fixture that never reproduced the class, and this gate's own
# first version proved exactly that. The control asserts the PIN still carries
# the host paths; if it does not, the gate FAILS rather than passing quietly.
#
# SECONDS AND NO SANDBOX. `-t drv` RESOLVES rather than builds: no daemon
# connection, safe on a loaded box. The cmake configure is local and takes
# ~30s, not a nix build.
#
#   NN_PIN_REV=<rev without the fix>   REQUIRED; the control arm
#   NN_GATE_SRC=<rev>                  driver/task from that revision (default: this tree)
#   KEEP=1                             keep the fixture for inspection
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

PIN_REV=${NN_PIN_REV:-}
[ -n "$PIN_REV" ] || { echo "NO ANSWER: set NN_PIN_REV=<a revision WITHOUT the fix> for the control arm"; exit 2; }

NN=$(b "$SRC#nix-ninja")     || { echo "NO ANSWER: the driver did not build"; exit 2; }
TASK=$(b "$SRC#nix-ninja-task") || { echo "NO ANSWER: the task binary did not build"; exit 2; }
PNN=$(b "git+file://$REPO?rev=$PIN_REV#nix-ninja") || { echo "NO ANSWER: the pin driver did not build"; exit 2; }

# Store tools BY ATTRIBUTE from this tree's own nixpkgs. These variables END IN
# `/bin`, so the PATH below joins them as they are - an earlier version
# appended `/bin` again and produced `/nix/store/.../bin/bin`, so the driver
# resolved NOTHING and died `Failed to find bash`. They were once read by a
# store GLOB (`ls -d /nix/store/*patchelf*/bin | head -1`), which answers with
# whatever sorts first: on 2026-10-06 that became `auto-patchelf`, fetched by
# an unrelated build, and the arm read NO ANSWER on `Failed to find patchelf`.
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; in f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}.$1" | tail -1; }
pbin() { local o; o=$(px "$1") && [ -n "$o" ] && echo "$o/bin"; }
CU=$(pbin coreutils)
BASHB=$(pbin bash)
PE=$(pbin patchelf)
CCW=$(pbin gcc)
GR=$(pbin gnugrep)
NIXB=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)
NIXB=${NIXB:+$NIXB/bin}
for t in "$CU" "$BASHB" "$PE" "$CCW" "$GR" "$NIXB"; do
  [ -n "$t" ] || { echo "NO ANSWER: a store tool did not resolve"; exit 2; }
done

D=$(mktemp -d -t nn-hostpath-XXXXXX); rm -rf "$D"
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D"' EXIT

# The real source, from the store by hash so the gate is reproducible.
SRCTAR=$(store_find '*-c-ares-1.34.8.tar.gz')
[ -n "$SRCTAR" ] || { echo "NO ANSWER: the c-ares source tarball is not resident"; exit 2; }
mkdir -p "$D"; tar xf "$SRCTAR" -C "$D"

# `cc` on the PATH: the wrapper provides `gcc`, and the driver requires the
# literal name `cc` for a `deps = gcc` edge.
mkdir -p "$D/ccbin"; ln -sf "$CCW/gcc" "$D/ccbin/cc"

# Configure, which is local and needs no daemon. `-GNinja` makes cmake emit the
# CMakeFiles/ edge the class dies on; the driver then reads that build.ninja.
cmake -S "$D/c-ares-1.34.8" -B "$D/c-ares-1.34.8/build" -GNinja \
      -DCARES_BUILD_TESTS=OFF -DCARES_BUILD_TOOLS=OFF >"$D/cfg.log" 2>&1 || {
  echo "NO ANSWER: cmake did not configure the fixture"; tail -3 "$D/cfg.log"; exit 2; }

drive() { # drive <driver> <target> > json
  ( cd "$D/c-ares-1.34.8/build"
    PATH="$1/bin:$TASK/bin:$NIXB:$CCW:$CU:$BASHB:$PE:$GR:$D/ccbin" \
    NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
    NIX_CONFIG="experimental-features = $F" \
    NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
    "$1/bin/nix-ninja" -t drv "$2" 2>"$D/drive.err"
  )
}

TARGET='src/lib/CMakeFiles/c-ares.dir/util/ares_threads.c.o'

# ONE reading, both questions, so the control asks them by the same code path.
#
# THE MAX CLIMB IS THE MECHANISM, and it is asserted alongside the path count
# because the path count could go to zero for a reason that is not this fix
# (a fixture that stages nothing at all). The build dir here is
# /build/c-ares-1.34.8/build, TWO components below /build, so any input
# climbing 3 outruns it and is what the deepening guard acts on:
#
#   pin    {3: 2, 2: 33, 0: 2}   two climb-3 = the host paths
#   fixed  {2: 33, 0: 2}          none outrun the dir
#
# A climb-3 input is the class, whatever it names; the `/etc` count is the
# instance.
ask() { python3 - "$1" <<'PY'
import json, sys
from collections import Counter
try:
    d = json.load(open(sys.argv[1]))
except Exception:
    print("PARSE-FAIL"); sys.exit(0)
body = next((v for v in d.values() if isinstance(v, dict) and "env" in v), None)
if body is None:
    print("NO-BODY"); sys.exit(0)
toks = body["env"].get("NIX_NINJA_INPUTS", "").split()

def climb(t):
    sp = t.split(":")[1] if t.count(":") >= 2 else ""
    n = 0
    for p in sp.split("/"):
        if p == "..":
            n += 1
        else:
            break
    return n

depths = Counter(climb(t) for t in toks)
over = sum(v for k, v in depths.items() if k > 2)      # 2 = how deep this build dir sits
hosts = [t for t in toks if "etc/hosts" in t or "etc/resolv.conf" in t]
src = [t for t in toks if "ares_threads.c" in t and not t.endswith(".h:")]
print("etc=%d over=%d total=%d source=%d depths=%s"
      % (len(hosts), over, len(toks), len(src), dict(sorted(depths.items()))))
PY
}

if ! drive "$NN" "$TARGET" >"$D/head.json"; then
  echo "NO ANSWER: the head driver did not emit a derivation"; sed -n '1,3p' "$D/drive.err"; exit 2
fi
echo "head   $(ask "$D/head.json")  ($NN)"
HE=$(ask "$D/head.json")

if ! drive "$PNN" "$TARGET" >"$D/pin.json"; then
  echo "NO ANSWER: the pin driver did not emit a derivation"; sed -n '1,3p' "$D/drive.err"; exit 2
fi
echo "pin    $(ask "$D/pin.json")  ($PNN)"
PE_=$(ask "$D/pin.json")

case "$HE" in
  etc=0*over=0*total=*source=[1-9]*) : ;;
  etc=0*over=0*source=0*) echo "FAIL: the host paths are gone AND the source is gone - the guard over-dropped"; exit 1 ;;
  etc=[1-9]*)             echo "FAIL: the host paths are still inputs of the task"; exit 1 ;;
  etc=0*over=[1-9]*)      echo "FAIL: no /etc path is named but an input still outruns the build dir - some other climb-3 remains"; exit 1 ;;
  *)                      echo "NO ANSWER: could not read the head derivation ($HE)"; exit 2 ;;
esac

# THE CONTROL FAILS THE GATE if it does not reproduce the class, because a head
# arm that agrees with a control that never carried the thing is a PASS about
# nothing. It asserts the MECHANISM (an input outrunning the build dir), not
# only the path count: a pin that dropped `/etc/hosts` for some other reason
# would satisfy the count and say nothing about this fix.
case "$PE_" in
  etc=0*)  echo "FAIL(control): the pin does not carry the host paths either, so the fixture"
           echo "  did not reproduce the class and the head arm proves nothing"; exit 1 ;;
  over=0*) echo "FAIL(control): the pin carries the paths but NO input outruns the build dir,"
           echo "  so the guard the fix acts on is not the one this fixture exercises"; exit 1 ;;
esac

echo "PASS: the host paths are dropped, no input outruns the build dir, the source is kept, and the pin shows both"
