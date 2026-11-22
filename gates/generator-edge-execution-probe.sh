#!/usr/bin/env bash
# Does any GENERATOR EDGE execute a produced artifact that links a nix-ninja
# intermediate?
#
# The answer decides whether an intermediate is on the build-time EXECUTION
# path or only on the dependency graph.
#
# CONFIGURATION B, and the letter is the point. B is local mode inside an
# ordinary recursive-nix derivation, which is the consumer's route. A result
# taken under A or C answers a different question and must not be recorded
# against this one.
#
# WHAT IS ALREADY KNOWN, and it is not the measurement:
# `nix-ninja-task`'s `fix_rpaths`/`assemble_rpath` resolve an output ELF's
# NEEDED libraries to the store directories of SIBLING per-TU derivations and
# write them into that output's RPATH. That work only pays off if the artifact
# is LOADED, and an artifact that only ever sat on a dependency graph is never
# loaded. So the module is evidence about INTENT, and no absence of a corpus
# weakens it. It is still not a measured edge, which is what this script is.
#
# STATE THE POPULATION BEFORE THE RESULT. A zero over an unstated denominator
# reads identically to a selection that matched nothing because it was looking
# in the wrong place. This prints considered/selected/examined before any
# verdict, and prints them even when the verdict is zero.
#
# WHAT A FALSE PASS LOOKS LIKE, because two different findings wear the same
# output. `patchelf --print-needed` against a STATICALLY SELF-CONTAINED
# artifact answers cleanly with an empty list and says nothing about the
# question. That is not "no sibling intermediate is linked" - it is "this
# artifact could not have told you either way". The two are counted in
# separate buckets below and must never be added together.
#
# SMOKE-TESTED 2026-09-04 IN CONFIGURATION A, ON A THROWAWAY DIRECTORY, AND
# THREE DEFECTS FELL OUT BEFORE THE FIRST REAL RUN. Each would have made the
# configuration-B result wrong in a different direction:
#   1. the output asserted "configuration: B" as a LITERAL; it is read from
#      the environment now.
#   2. the selector read only `command =` lines; meson and CMake put
#      `$COMMAND` there and the real command on a `COMMAND =` line, often
#      after `cd <dir> &&` and as an absolute path under the build dir, all
#      of which it discarded. On dtc it would have selected ZERO and printed
#      NO ANSWER, read as a missing precondition.
#   3. "lands in the store" fired on glibc, which every nix-gcc artifact
#      resolves through a store RUNPATH, so B would have answered YES on any
#      input. A nix-ninja intermediate is a task output named
#      `<hash>-ninja-build-<name>`; the toolchain has its own bucket.
# Measured after the repairs, three arms, exit status read as its own
# value: real store, zlib-linked artifact -> toolchain 1, intermediate 0,
# NO, rc=1; NN_PROBE_STORE pointed at a scratch tree holding
# `abc123-ninja-build-libfake` -> intermediate 1, YES, rc=0; no directory
# -> rc=2. Three command spellings selected: `./gen`, `cd D && /abs/st`,
# `cd sub && ./pos`. Configuration B is the run that answers the question;
# a wrapper that builds a real package (dtc, later orc) inside recursive-nix
# and hands this probe its kept build directory takes it.
#
# FOURTH DEFECT, found on the first real run (dtc, configuration B,
# 2026-09-04 17:13): the bucket loop left the artifact after the FIRST
# NEEDED library that resolved anywhere in the store, so glibc's landing
# was counted and libfdt.so.1, one line later, was never resolved; and
# $ORIGIN was expanded to the store object behind the placed symlink,
# where `../libfdt` does not exist, instead of the build-directory path the
# generator task executes. dumptrees links libfdt through
# `-rpath,$ORIGIN/../libfdt` and the probe read it as toolchain-only.
# Every NEEDED library is now resolved at the execution location and the
# landing printed per library. Smoke: subject YES (rc 0), control with the
# same library under a non-intermediate store name NO (rc 1).
#
# FIFTH DEFECT, found on orc (configuration B, 2026-09-04 17:29): after
# the driver's mtime refresh a placed product is a COPY, so a library
# reached through $ORIGIN landed outside the store and read as "outside"
# while the artifact's RPATH carried the store intermediate directory that
# assemble_rpath appends for every needed sibling. A library that lands
# outside the store is now also looked for in every RPATH entry under
# $STORE/*-ninja-build-*, and a hit is INTERMEDIATE. Smoke: copy plus
# store entry YES (rc 0); same copy with the store entry renamed NO (rc 1).
#
# SELECTION IS STATIC. An edge is selected from build.ninja and its
# artifact examined if it exists on disk; nothing here shows the edge RAN.
# On dtc the artifact existed because `all` builds dumptrees, while the
# custom target that executes it is a test dependency outside `all`, so
# a run that never asked for a .dtb output examined an artifact no edge
# had executed. The caller must prove execution separately (the B wrapper
# lists the generator's outputs) and request the target by name.
#
# Usage:  generator-edge-execution-probe.sh <build-dir-containing-build.ninja>
# Does NOT manufacture a build directory. Run it when a round leaves one.

set -uo pipefail

BUILD_DIR="${1:-}"
if [[ -z "$BUILD_DIR" || ! -f "$BUILD_DIR/build.ninja" ]]; then
    echo "usage: $0 <build-dir with build.ninja>" >&2
    echo "no build directory given: NOT a negative result, the probe did not run" >&2
    exit 2
fi

command -v patchelf >/dev/null || { echo "patchelf absent: probe cannot run" >&2; exit 2; }

# The store prefix is a PARAMETER so a positive control can exist: the
# smoke fixture points it at a scratch directory holding a
# `<hash>-ninja-build-<name>` tree, since nothing can write a real one.
STORE=${NN_PROBE_STORE:-/nix/store}

cd "$BUILD_DIR" || exit 2

# The configuration is in the output header because a result carries the
# letter it was taken on or it carries nothing. It is
# READ, not asserted: NIX_BUILD_TOP set with no builder-rpc feature is B,
# unset is A. C cannot run this at all, it has no build directory to read.
if [[ -n "${NIX_BUILD_TOP:-}" ]]; then cfg=B; else cfg=A; fi
echo "configuration: $cfg   (B is the consumer's route; an A result answers a different question)"
echo "build dir:     $BUILD_DIR"

# Every command line in the graph. A rule's `command =` is usually
# `$COMMAND` under meson and CMake, with the real command on a `COMMAND =`
# line beneath each build statement, so both spellings are read. A smoke
# fixture that spelled the artifact inside the rule passed a selector that
# read only the rule, which is the fixture shape the audit file names.
mapfile -t cmds < <(grep -E '^\s+(command|COMMAND) = ' build.ninja | sed -E 's/^\s*(command|COMMAND) = //')
considered=${#cmds[@]}

# A generator edge for this question invokes something UNDER THE BUILD
# DIRECTORY rather than a toolchain binary. A store path or an absolute path
# outside the build directory is a tool; a relative token is resolved
# against the directory a leading `cd <dir> &&` selected, which is how CMake
# spells every custom command.
selected=()
for c in "${cmds[@]}"; do
    base="$PWD"
    set -- $c
    while (( $# )); do
        tok=$1; shift
        case "$tok" in
            cd) [[ "${1:-}" ]] && { base=$(cd "$1" 2>/dev/null && pwd || echo "$base"); shift; }
                continue ;;
            '&&'|';'|'||') continue ;;
            "$STORE"/*) break ;;
            /*) [[ "$tok" == "$PWD"/* ]] || break
                cand=$tok ;;
            ./*|*/*) cand="$base/$tok" ;;
            *) break ;;
        esac
        [[ -f "$cand" && -x "$cand" ]] && selected+=("$cand")
        break
    done
done

# Deduplicate: one artifact invoked by twenty edges is one artifact.
mapfile -t artifacts < <(printf '%s\n' "${selected[@]:-}" | grep -v '^$' | sort -u)

examined=0; dynamic=0; static_selfcontained=0; sibling_linked=0; toolchain_linked=0
declare -a hits

for a in "${artifacts[@]}"; do
    [[ -n "$a" ]] || continue
    examined=$((examined+1))
    needed=$(patchelf --print-needed "$a" 2>/dev/null)
    if [[ -z "$needed" ]]; then
        # FALSE-PASS BUCKET. Says nothing about the question.
        static_selfcontained=$((static_selfcontained+1))
        continue
    fi
    dynamic=$((dynamic+1))
    rpath=$(patchelf --print-rpath "$a" 2>/dev/null)
    # $ORIGIN is where the artifact sits WHEN EXECUTED: the generator task
    # runs it at its build-directory path, so $ORIGIN is that directory,
    # never the store object a placed symlink points into. Every NEEDED
    # library is resolved, and the artifact is bucketed once: a sibling
    # per-TU derivation is a task output named `<hash>-ninja-build-<name>`,
    # reached directly or through a placed symlink; anything else in the
    # store is the toolchain (glibc, libstdc++), which under configuration B
    # every dynamic artifact links and which answers nothing.
    origin=$(dirname "$a")
    has_sibling=0; has_toolchain=0
    echo "artifact: $a"
    echo "  rpath:  ${rpath:-<none>}"
    while IFS= read -r lib; do
        [[ -n "$lib" ]] || continue
        landing=""
        IFS=':' read -ra dirs <<< "$rpath"
        for d in "${dirs[@]}"; do
            [[ -n "$d" ]] || continue
            cand="${d//\$ORIGIN/$origin}/$lib"
            [[ -e "$cand" ]] || continue
            landing=$(readlink -f "$cand")
            break
        done
        # A placed product is a COPY after the driver's mtime refresh, so a
        # library reached through $ORIGIN lands outside the store even when
        # it is a task output. The task binary's assemble_rpath appends the
        # store directory of every needed sibling, so that entry is the
        # evidence: a store intermediate directory in the RPATH holding the
        # same library name is the sibling, wherever $ORIGIN landed.
        via_store=""
        for d in "${dirs[@]}"; do
            [[ "$d" == "$STORE"/*-ninja-build-* && -e "$d/$lib" ]] && { via_store=$d; break; }
        done
        case "$landing" in
            "") echo "  needed: $lib -> unresolved through RPATH" ;;
            "$STORE"/*-ninja-build-*) echo "  needed: $lib -> $landing   INTERMEDIATE"; has_sibling=1 ;;
            "$STORE"/*) echo "  needed: $lib -> $landing   toolchain"; has_toolchain=1 ;;
            *) if [[ -n "$via_store" ]]; then
                   echo "  needed: $lib -> $landing (placed copy); also in RPATH at $via_store   INTERMEDIATE"; has_sibling=1
               else
                   echo "  needed: $lib -> $landing   outside the store"
               fi ;;
        esac
    done <<< "$needed"
    if (( has_sibling )); then
        sibling_linked=$((sibling_linked+1)); hits+=("$a")
    elif (( has_toolchain )); then
        toolchain_linked=$((toolchain_linked+1))
    fi
done

echo "--- population, stated before the result ---"
echo "command lines considered: $considered"
echo "generator edges selected: ${#selected[@]}"
echo "distinct artifacts:       ${#artifacts[@]}"
echo "artifacts examined:       $examined"
echo "--- buckets, never summed ---"
echo "dynamically linked:       $dynamic"
echo "statically self-contained:$static_selfcontained   <- FALSE-PASS bucket, tells you nothing"
echo "linking a TOOLCHAIN store lib:$toolchain_linked   <- glibc, libstdc++; not the question"
echo "linking a nix-ninja intermediate ($STORE/*-ninja-build-*): $sibling_linked"
echo "--- verdict ---"
if (( examined == 0 )); then
    echo "NO ANSWER: nothing was examined. This is a missing precondition, not a negative result."
    exit 2
fi
if (( sibling_linked > 0 )); then
    echo "YES: a generator edge executes an artifact linking a store-resident sibling."
    printf '  %s\n' "${hits[@]}"
    exit 0
fi
if (( dynamic == 0 )); then
    echo "NO ANSWER: every artifact examined was statically self-contained."
    echo "The probe could not discriminate. Do not record this as 'no such edge'."
    exit 2
fi
echo "NO such edge found, over the stated denominator, with $dynamic dynamic artifact(s) that COULD have shown one."
exit 1
