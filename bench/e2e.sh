#!/usr/bin/env bash
# End-to-end compilation benchmark - upstream nix-ninja #7.
#
# #7 asks for benchmarks of end-to-end compilation "for perf work upstream".
# The driver already prints everything such work needs; what has never existed
# is something that RUNS a build and records those numbers in a form two
# people can compare. Every figure this project has argued about was produced
# ad hoc into a log and then quoted from memory.
#
# This runs one example end to end and emits a single record: wall clock, the
# driver's own phase breakdown, and the store paths, so a later run can be
# diffed against it.
#
# Usage:  bench/e2e.sh example-hello [outfile.json]
#
# It does NOT clean the store first, so a record is only comparable with
# another record built the same way. Two fields say how much work actually
# happened, and BOTH are needed:
#
#   target_prebuilt   the target itself was already realised
#   derivations_built how many derivations the run actually built
#
# target_prebuilt alone is not enough and saying so cost a wrong reading here:
# two runs of example-hello both reported it false - the target had re-keyed
# each time, so it genuinely was not built - while taking 312 s and 18.6 s,
# because the first built nix's whole closure and the second reused it. False
# does not mean cold.
set -uo pipefail

TARGET="${1:?usage: bench/e2e.sh <example-name> [outfile]}"
OUT="${2:-bench-$TARGET-$(date +%Y%m%dT%H%M%S).json}"
FEATURES='nix-command flakes dynamic-derivations ca-derivations recursive-nix'

# These examples evaluate to a STRING, not a derivation: each is
# `builtins.outputOf ninjaDrv.outPath <target>`, which is the whole point of
# the dynamic-derivation design. So there is no `.drvPath` to read, and the
# attribute is addressed bare - `.#example-hello` - exactly as the README and
# CLAUDE.md say to build it.
drv=$(nix eval --raw --extra-experimental-features "$FEATURES" \
        ".#${TARGET}" 2>/dev/null)
if [ -z "$drv" ]; then
  echo "cannot evaluate .#${TARGET}" >&2
  exit 2
fi

# Was the target already built? Read BEFORE building, or the answer is
# always yes.
# Three-valued on purpose. Any path-info failure - daemon down, feature
# rejected, malformed path - used to be recorded as `false`, which is the
# direction that reads as "more work was done" and is exactly the wrong way
# for an unknown to fail.
prebuilt=unknown
pi_err=$(nix path-info --extra-experimental-features "$FEATURES" "$drv" 2>&1 >/dev/null)
pi_rc=$?
if [ "$pi_rc" -eq 0 ]; then
  prebuilt=true
elif printf '%s' "$pi_err" | grep -qi 'does not exist\|is not valid\|no such'; then
  prebuilt=false
fi

# The driver emits `nix-ninja-stats` unconditionally - it runs inside a
# derivation, so there is no environment variable this script could set that
# would reach it.
start=$(date +%s.%N)
nix build --extra-experimental-features "$FEATURES" --no-link --print-build-logs \
  ".#${TARGET}" >"$OUT.log" 2>&1
rc=$?
end=$(date +%s.%N)
wall=$(awk -v a="$start" -v b="$end" 'BEGIN{printf "%.3f", b-a}')

# The driver's own accounting, from the build log. One line per driver run;
# take the last, which is the completed one.
# How much was actually built, as opposed to substituted or already present.
# `grep -c` prints 0 and exits 1, so take the count and discard the status.
# null, not 0, when the log is missing or unreadable: "nothing was built" and
# "the count could not be taken" are different facts and 0 asserts the first.
# The anchor is checked against real logs - nix does not prefix these lines.
if [ -r "$OUT.log" ]; then
  built=$(grep -c "^building '" "$OUT.log" 2>/dev/null) || true
  built=${built:-0}
else
  built=null
fi

# A FAILED BUILD MUST NOT LOOK LIKE A RESULT. wall_seconds and
# derivations_built are the fields a comparison reads, and a run that died in
# four seconds has both. Divert the record so no later sweep of *.json can
# average a fast failure into a fast success.
# THE LOG KEEPS THE NAME IT WAS WRITTEN UNDER. Reassigning OUT before the
# greps below made them read "$OUT.failed.log", which nothing ever writes, so
# every FAILED run recorded driver_line: null and no stats - the diagnostics
# were dropped from precisely the runs that need them. Found 2026-08-30 when
# example-nix failed and its record explained nothing.
LOG="$OUT.log"
if [ "$rc" -ne 0 ]; then
  OUT="$OUT.failed"
  echo "build failed (rc=$rc); record written to $OUT (log: $LOG)" >&2
  # The last lines of the failure, so the record is not the only artifact and
  # the reader is not told to go find a file.
  tail -25 "$LOG" >&2 2>/dev/null || true
fi

resolved=$(grep -h 'nix-ninja: resolved' "$LOG" 2>/dev/null | tail -1)
stats=$(grep -ho 'nix-ninja-stats {.*}' "$LOG" 2>/dev/null | tail -1)
stats="${stats#nix-ninja-stats }"

python3 - "$OUT" "$TARGET" "$wall" "$rc" "$prebuilt" "$drv" "$resolved" "$stats" "$built" <<'PY'
import json, sys, re
out, target, wall, rc, prebuilt, drv, resolved = sys.argv[1:8]
stats = sys.argv[8] if len(sys.argv) > 8 else ""
built = sys.argv[9] if len(sys.argv) > 9 else "0"
rec = {
    "target": target,
    "wall_seconds": float(wall),
    "exit_code": int(rc),
    "target_prebuilt": {"true": True, "false": False}.get(prebuilt),
    "derivations_built": None if built == "null" else int(built),
    "drv": drv,
    "driver_line": resolved or None,
}
# Millisecond counters straight from the driver when it emitted them. These
# are the authority; the regex fallback below reads the human line, which is
# in whole seconds and rounds a short build to nothing.
if stats:
    try:
        rec["stats_ms"] = json.loads(stats)
    except ValueError:
        rec["stats_ms"] = None

# Parse the phases the driver prints, so a diff is per-phase and not just
# wall clock. Absent keys mean the line was absent, NOT that a phase was zero.
if resolved:
    for key, pat in [
        ("tasks", r"resolved (\d+) tasks"),
        ("resolve_s", r"(\d+) s total resolve time"),
        ("dyn_s", r"dyn (\d+) s"),
        ("dyn_realise_s", r"realise (\d+) s"),
        ("dyn_discover_s", r"discover (\d+) s"),
        ("plain_adddrv_s", r"plain adddrv (\d+) s"),
        ("plain_adddrv_calls", r"plain adddrv \d+ s/(\d+) calls"),
        ("rss_mib", r"rss (\d+) MiB"),
    ]:
        m = re.search(pat, resolved)
        if m:
            rec[key] = int(m.group(1))
with open(out, "w") as fh:
    json.dump(rec, fh, indent=2, sort_keys=True)
print(json.dumps(rec, indent=2, sort_keys=True))
PY
# The python above writes the record. If IT failed, the record does not
# exist and the caller must not read our exit status as "record written".
py_rc=$?
if [ "$py_rc" -ne 0 ]; then
  echo "failed to write record to $OUT (python rc=$py_rc)" >&2
  exit 3
fi

exit $rc
