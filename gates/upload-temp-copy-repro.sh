#!/usr/bin/env bash
# Configuration A, seconds. An edge reads three inputs the driver uploads
# from a REWRITTEN copy: an env-shebang script (`patched_env_shebang`), a
# file naming an outer output path (`outer_rewrite_map`) and a CMake
# dependency-info file naming the build directory (`is_cmake_depend_info`).
# Each copy is written under the driver's temp directory, which on
# configuration A is the host's. PASS when the temp directory holds none
# of them after the drive; `--control <rev>` drives the same tree with that revision's
# driver and expects the leak (reproduces on f2dba47).
#
# What a FALSE PASS looks like: the script is uploaded WITHOUT a rewrite
# (not executable, or the shebang is not the env form), so no copy is ever
# written. The gate therefore also requires the rewritten copy to have
# been uploaded: `-t drv` must name a store copy with a store-path shebang.
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd); SRC=$REPO
expect=none
if [ "${1:-}" = --control ]; then
  W=$(mktemp -d -t nn-utc-wt-XXXXXX); rmdir "$W"
  git -C "$REPO" worktree add -q --detach "$W" "${2:?rev}" || exit 2
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  SRC=$W; expect=leak; echo "driver from $2"
fi
b() { nix build --extra-experimental-features "$F" --cores 4 --max-jobs 1 --no-link --print-out-paths "$@" | tail -1; }
px() { b --impure --expr "let f=builtins.getFlake \"$REPO\"; p=f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1"; }
NN=$(b "$SRC#nix-ninja") || exit 2; TASK=$(b "$SRC#nix-ninja-task") || exit 2
CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); CC=$(px gcc)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default")
D=$(mktemp -d -t nn-utc-XXXXXX); mkdir -p "$D/build" "$D/tmp"; cd "$D/build" || exit 2
# Three writers, one input each: an env-shebang script, a file naming an
# outer output path (nn-outer copy), and a CMake dependency-info file
# naming the build directory absolutely (nn-tdi copy).
OUT=/nix/store/00000000000000000000000000000000-outer
printf '#!/usr/bin/env bash\ncat cfg.txt x.dir/FortranDependInfo.json > "$1"\n' > gen.sh; chmod 755 gen.sh
printf 'datadir=%s/share\n' "$OUT" > cfg.txt
mkdir -p x.dir; printf '{"dir-cur-bld":"%s"}\n' "$D/build" > x.dir/FortranDependInfo.json
cat > build.ninja <<'NINJA'
rule gen
  command = ./gen.sh $out
build out.txt: gen | gen.sh cfg.txt x.dir/FortranDependInfo.json
default out.txt
NINJA
env -i HOME="$D" TMPDIR="$D/tmp" outputs=out out="$OUT" PATH="$NN/bin:$CU/bin:$BASH/bin:$NIXP/bin:$TASK/bin:$PE/bin:$CC/bin" \
  NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" NIX_CONFIG="experimental-features = $F" \
  "$NN/bin/nix-ninja" -v out.txt > "$D/log" 2>&1; rc=$?
echo "drive rc=$rc"; [ -s out.txt ] || { echo "FAIL: out.txt not generated; kept $D"; exit 1; }
# The rewrite must have happened, or a clean temp dir proves nothing: the
# task derivation's upload of gen.sh must carry a store-path shebang.
g=$(env -i HOME="$D" TMPDIR="$D/tmp" outputs=out out="$OUT" PATH="$NN/bin:$CU/bin:$BASH/bin:$NIXP/bin:$TASK/bin:$PE/bin:$CC/bin" \
  NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" NIX_CONFIG="experimental-features = $F" \
  "$NN/bin/nix-ninja" -t drv out.txt > "$D/drv.json" 2>>"$D/log")
up() { command grep -o "/nix/store/[a-z0-9]*-$1" "$D/drv.json" | sort -u | head -1; }
g=$(up 'gen\.sh'); c=$(up 'cfg\.txt'); t=$(up 'FortranDependInfo\.json')
case $(head -1 "${g:-/dev/null}") in
  '#!/nix/store/'*) echo "uploaded copy: $g (store-path shebang)" ;;
  *) echo "FAIL: no rewritten upload of gen.sh in the task derivation; kept $D"; exit 1 ;;
esac
[ -n "$c" ] && ! command grep -q "$OUT" "$c" || { echo "FAIL: cfg.txt upload ${c:-missing} still names the outer path; kept $D"; exit 1; }
[ -n "$t" ] && ! command grep -q "$D/build" "$t" || { echo "FAIL: dependency-info upload ${t:-missing} still names the host build dir; kept $D"; exit 1; }
echo "uploaded copies: $c (placeholder), $t (sandbox build dir)"
# A generator edge names no toolchain program, so its task derivation must
# not carry the compiler wrapper (command_names_toolchain): 27 store paths
# out of its realise closure.
if command grep -q 'gcc-wrapper' "$D/drv.json"; then
  echo "FAIL: the generator task carries the compiler wrapper; kept $D"; exit 1
fi
echo "task derivation: no compiler wrapper (generator edge)"
n=$(find "$D/tmp" -maxdepth 1 \( -name 'nn-shebang-*' -o -name 'nix-ninja-shebang-*' -o -name 'nn-outer-*' -o -name 'nn-tdi-*' \) | wc -l)
echo "temp entries after the drive: $n"; find "$D/tmp" -maxdepth 1 | sed 1d | sed 's/^/  /' | head -5
case $expect in
  none) if [ "$n" -eq 0 ]; then echo "PASS: no rewritten copy left in the temp directory"; [ -n "${KEEP:-}" ] || rm -rf "$D"; exit 0; fi
        echo "FAIL: $n copies left; kept $D"; exit 1 ;;
  leak) if [ "$n" -gt 0 ]; then echo "CONTROL PASS: the pre-fix driver leaves $n entries"; rm -rf "$D"; exit 0; fi
        echo "CONTROL FAILED: expected a leak, found none; kept $D"; exit 1 ;;
esac
