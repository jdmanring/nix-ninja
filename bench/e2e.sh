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
# It does NOT clean the store first. Timing a build whose outputs are already
# banked measures substitution, not compilation, so the record says which it
# was rather than pretending: `prebuilt` is true when the target was already
# realised, and a prebuilt run is not comparable with a cold one.
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

# Was it already built? Read BEFORE building, or the answer is always yes.
prebuilt=false
if nix path-info --extra-experimental-features "$FEATURES" "$drv" >/dev/null 2>&1; then
  prebuilt=true
fi

start=$(date +%s.%N)
nix build --extra-experimental-features "$FEATURES" --no-link --print-build-logs \
  ".#${TARGET}" >/dev/null 2>"$OUT.log"
rc=$?
end=$(date +%s.%N)
wall=$(awk -v a="$start" -v b="$end" 'BEGIN{printf "%.3f", b-a}')

# The driver's own accounting, from the build log. One line per driver run;
# take the last, which is the completed one.
resolved=$(grep -h 'nix-ninja: resolved' "$OUT.log" 2>/dev/null | tail -1)

python3 - "$OUT" "$TARGET" "$wall" "$rc" "$prebuilt" "$drv" "$resolved" <<'PY'
import json, sys, re
out, target, wall, rc, prebuilt, drv, resolved = sys.argv[1:8]
rec = {
    "target": target,
    "wall_seconds": float(wall),
    "exit_code": int(rc),
    "prebuilt": prebuilt == "true",
    "drv": drv,
    "driver_line": resolved or None,
}
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
json.dump(rec, open(out, "w"), indent=2, sort_keys=True)
print(json.dumps(rec, indent=2, sort_keys=True))
PY
exit $rc
