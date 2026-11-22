#!/usr/bin/env bash
# The symlink-mtime rebuild class, end to end, with its own negative control.
#
# `local.rs` materializes a build product as a symlink into the store, and nix
# gives every store file mtime 1. `make` stats THROUGH the symlink, so every
# object the driver placed reads as older than every source and the whole tree
# is out of date on the next pass. Measured across one distribution round:
# guile 135 objects compiled ONCE, gnutar 199 TWICE, bison 166 FOUR times,
# uniform within each package and tracking how many passes each build makes.
#
# That measurement is configuration B and cannot be repeated without a round.
# Reproduced here in configuration A at a ten-thousandth the size: seven objects
# (six units and main) under one Makefile, each compile handed to the driver as a one-edge build
# file the way a compiler drop-in hands it, built twice. The unit is the one the round used - COMPILES PER OBJECT - because
# mtime cannot serve as the detector in the control arm: a recompile under the
# unfixed driver replaces the symlink with another symlink to the same store
# path, so the observable time does not move even though the compile happened.
#
#   fixed driver     -> pass 2 compiles 0 of 7
#   pre-fix driver   -> pass 2 compiles 7 of 7
#
# The second arm is the point. A `make` whose dependency check never engages
# reports 0 of 7 with or without the fix, and a passing subject arm is then
# indistinguishable from an inert harness.
#
#   gates/mtime-rebuild-repro.sh <driver-bin> [label]
#
# <driver-bin> is a store path (`nix build .#nix-ninja --print-out-paths`
# plus /bin/nix-ninja). Exit 0 when pass 2 compiles nothing, 1 when it
# rebuilds, 3 or 4 when the harness itself failed.
#
# Needs: a builder-rpc-v0 daemon, and the experimental features on the
# invocation rather than machine-wide. Run from a checkout.
set -euo pipefail
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
NN=${1:?usage: mtime-rebuild-repro.sh <driver-bin> [label]}
LABEL=${2:-unlabelled}
[ -x "$NN" ] || { echo "not executable: $NN" >&2; exit 2; }

b() { nice -n 10 nix build --extra-experimental-features "$F" --no-link --print-out-paths "$@"; }
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; p = f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }
TASK=$(b "$REPO#nix-ninja-task")
CC=$(px gcc); BU=$(px binutils); CU=$(px coreutils); BASH=$(px bash)
PE=$(px patchelf); MAKE=$(px gnumake); SED=$(px gnused); DIFF=$(px diffutils)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

TOP=$(mktemp -d -t nn-mtime-XXXXXX); trap 'chmod -R u+w "$TOP" 2>/dev/null; rm -rf "$TOP"' EXIT
SRC=$TOP/src; BIN=$TOP/bin; mkdir -p "$SRC" "$BIN"

# Six translation units and a Makefile that links them. Nothing here is
# generated, so a second `make` has no honest reason to compile anything.
for i in 1 2 3 4 5 6; do printf 'int u%s(void){return %s;}\n' "$i" "$i" > "$SRC/u$i.c"; done
printf 'extern int u1(void),u2(void),u3(void),u4(void),u5(void),u6(void);\nint main(void){return u1()+u2()+u3()+u4()+u5()+u6()==21?0:1;}\n' > "$SRC/main.c"
cat > "$SRC/Makefile" <<'EOF'
OBJ = u1.o u2.o u3.o u4.o u5.o u6.o main.o
app: $(OBJ)
	$(CC) $(OBJ) -o app
%.o: %.c
	$(CC) -c $< -o $@
EOF

# THE DROP-IN. A single-TU `gcc -c SRC -o OBJ` becomes a one-edge build file
# (the real compiler by store path, `deps = gcc`) and the driver runs it from
# the compile's directory with the build-dir walk off; anything else, here
# the link, is the real compiler. That is what a compiler drop-in does on
# this route, and the arguments this Makefile passes need no quoting. The
# drop-in is never on PATH, so the driver's PATH holds no compiler stand-in.
SHIM=$TOP/shim; mkdir -p "$SHIM" "$TOP/.nn-cc"
cat > "$SHIM/gcc" <<EOF
#!/usr/bin/env bash
obj= src= c=0 prev=
for a in "\$@"; do
  case "\$prev" in -o) obj=\$a ;; esac
  case "\$a" in -c) c=1 ;; *.c) src=\$a ;; esac
  prev=\$a
done
[ "\$c" = 1 ] && [ -n "\$obj" ] && [ -n "\$src" ] || exec "$CC/bin/gcc" "\$@"
f="$TOP/.nn-cc/\$obj.ninja"
printf 'rule cc\n  command = %s\n  deps = gcc\nbuild %s: cc %s\n' "$CC/bin/gcc \$*" "\$obj" "\$src" > "\$f"
NIX_NINJA_NO_BUILD_DIR_SCAN=1 exec "$NN" -C "\$PWD" -f "\$f" "\$obj"
EOF
chmod +x "$SHIM/gcc"
cat > "$BIN/gcc" <<EOF
#!/usr/bin/env bash
compile=0 obj=
for a in "\$@"; do
  case "\$prev" in -o) obj=\$a ;; esac
  case "\$a" in -c) compile=1 ;; esac
  prev=\$a
done
[ "\$compile" = 1 ] && [ -n "\$obj" ] && echo "\$obj" >> "$TOP/compiles"
exec "$SHIM/gcc" "\$@"
EOF
chmod +x "$BIN/gcc"

# NIX_BUILD_TOP STAYS UNSET. `connect_from_env` reads it to decide the driver
# is inside a derivation and then negotiates the builder-rpc protocol, which
# the ordinary daemon does not offer, so a drive with it set dies on
# `add-to-store-scanning` before any object is placed. This is configuration A.

run_pass() {
  : > "$TOP/compiles"
  ( cd "$SRC" && env PATH="$(dirname "$NN"):$CC/bin:$BU/bin:$CU/bin:$BASH/bin:$NIXP/bin:$TASK/bin:$PE/bin:$MAKE/bin:$SED/bin:$DIFF/bin" \
      NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
      NIX_CONFIG="extra-experimental-features = $F" \
      "$MAKE/bin/make" CC="$BIN/gcc" "$@" ) >"$TOP/pass.log" 2>&1 || {
        echo "make failed in pass; tail follows" >&2; tail -30 "$TOP/pass.log" >&2; exit 3; }
  sort "$TOP/compiles"
}

P1=$(run_pass); n1=$(printf '%s' "$P1" | grep -c . || true)
[ "$n1" -ge 7 ] || { echo "FAIL: pass 1 compiled $n1 objects, expected 7 - the harness never engaged" >&2; exit 4; }
[ -x "$SRC/app" ] || { echo "FAIL: pass 1 produced no binary" >&2; exit 4; }
P2=$(run_pass); n2=$(printf '%s' "$P2" | grep -c . || true)

echo "driver     $NN"
echo "label      $LABEL"
echo "pass 1     $n1 compiles"
echo "pass 2     $n2 compiles"
if [ "$n2" = 0 ]; then echo "VERDICT    no rebuild - the placed product reads newer than its source"
else echo "VERDICT    rebuilt $n2 - the placed product still reads older than its source"; exit 1; fi
