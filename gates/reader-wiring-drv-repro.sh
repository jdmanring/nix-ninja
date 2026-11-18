#!/usr/bin/env bash
# DOES A READER'S OUTPUT SURVIVE THE WIRING AND REACH THE EMITTED DERIVATION?
#
# This is the layer between a reader's UNIT TEST and the PACKAGE, and it is the
# one nobody had measured. The unit tests drive `swig_referenced_paths` and
# `msgfmt_its_referenced_paths` directly, so they prove the reader can produce a
# spelling. They do NOT prove the driver KEEPS it: a candidate goes through
# `rebase_post_cd`, then two absolute-path filters (`starts_with(store_dir)`
# and `!same_project_tree`), and an ABSOLUTE spelling is dropped in silence -
# the reader looks fixed and the task still fails. CLAUDE.md names this as the
# trap; until now no instrument exercised it.
#
# WHAT THIS ASSERTS. Run the driver's `-t drv` over a fixture whose edge carries
# only the declared file, and read NIX_NINJA_INPUTS on the emitted derivation.
# The reader's target must appear there, at a RELATIVE spelling. The PIN's
# driver is the control: it carries the declared file and NOT the target, so a
# run that passes here and at the pin would mean the fixture was already
# carrying the file another way and says nothing.
#
# SECONDS AND NO SANDBOX: `-t drv` RESOLVES rather than builds, so this is a
# resolution-level instrument. It says the derivation CARRIES the input; a real
# package build is still the number that decides. Two arms, one per reader,
# each scored against its own pin control.
#
#   PASS       both targets carried here and absent at the pin
#   FAIL       a target is not carried, or the pin carries it (fixture wrong)
#   NO ANSWER  a drive failed for a reason other than the class
set -u
# NEVER GLOB /nix/store: zsh or bash expanding `/nix/store/*...` reads and
# sorts every entry, and over 650k entries one expansion reached 77 GB RSS
# and stalled a live round (2026-10-17). Stream it and stop at the first hit.
store_find() {  # store_find NAME_PATTERN [SUBPATH]
  find /nix/store -maxdepth 1 -name "$1" ! -name '*.drv' -print 2>/dev/null |
    while IFS= read -r d; do
      [ -e "$d${2:-}" ] && { printf '%s\n' "$d${2:-}"; break; }
    done
}

FEAT='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")/../.." && pwd)
b() { nice -n 10 nix build --cores 8 --extra-experimental-features "$FEAT" \
        --no-link --print-out-paths "$@"; }
# THE CONTROL IS A REVISION WITHOUT THE READERS (`0a35a39`), built by revision.
# It was two literal store paths (37672a4's pair); the 2026-10-01 collect took
# them and the arm answered NO ANSWER from then on, so a path is not a control.
CTL_REV=$(git -C "$REPO" rev-parse "${NN_READER_CONTROL_REV:-37672a4}") || exit 2
PIN_DRV=${NN_PIN_DRV:-$(b "git+file://$REPO?rev=$CTL_REV#nix-ninja")}
PIN_TASK=${NN_PIN_TASK:-$(b "git+file://$REPO?rev=$CTL_REV#nix-ninja-task")}
# THE READER-WITHOUT-DEFERRAL CONTROL IS aaefd48: the qrc reader is present
# and the deferral is absent, so silence there isolates the deferral as the
# delta rather than the reader. Pinned by revision like the pin pair.
AAE_REV=$(git -C "$REPO" rev-parse "${NN_AAE_REV:-aaefd48}") || exit 2
AAE_DRV=${NN_AAE_DRV:-$(b "git+file://$REPO?rev=$AAE_REV#nix-ninja")}
AAE_TASK=${NN_AAE_TASK:-$(b "git+file://$REPO?rev=$AAE_REV#nix-ninja-task")}
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; p = f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }

SRC=${NN_GATE_SRC:-$REPO}
NN=$(b "$SRC#nix-ninja") || { echo "NO ANSWER: the driver did not build"; exit 2; }
TASK=$(b "$SRC#nix-ninja-task") || { echo "NO ANSWER: the task binary did not build"; exit 2; }
CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); CC=$(px gcc)
GR=$(px gnugrep); MESON=$(px meson); SWIG=$(px swig); PB=$(px protobuf)
# glib-mkenums is a python script in glib's -dev output, so the PATH entry
# is that output's bin, resolved from the package the fixture actually runs.
# glib-mkenums lives in the -dev output, NOT the -bin one `px glib` returns
# (that output holds the shared library). Resolved by asking the store.
MKENUMS_BIN=$(store_find '*-glib-2.88.3-dev' /bin/glib-mkenums)
[ -x "$MKENUMS_BIN" ] || { echo "NO ANSWER: glib-mkenums not in the store, so arm 6 cannot run"; exit 2; }
CM=$(px cmake); CMAKE_BIN="$CM/bin/cmake"; CMAKE_BIN_DIR="$CM/bin"
# ANY STORE NIX SERVES, AND BUILDING ONE IS NOT THE GATE'S JOB. `Tools::new`
# in the driver resolves `nix` from PATH and errors if it is absent, and any
# resident nix satisfies it - the driver only needs a binary of that name.
# This line used to be
#
#   NIXP=$(b --impure --expr "...inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)
#
# which BUILT the nix package, so a gate whose header promises SECONDS was a
# full package build - and on 2026-10-01 that build landed on a box already
# two-thirds through a bootstrap, three times, because a reader believes the
# header and not line 42. Prefer a resident one; fall back to the build only
# where none is, so the gate still works on a store that holds no nix yet.
NIXP=$(store_find '*nix-2*' /bin)
NIXP=${NIXP%/bin}
if [ -z "$NIXP" ] || [ ! -x "$NIXP/bin/nix" ]; then
  NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)
fi
for t in "$CU" "$BASH" "$PE" "$CC" "$MESON" "$SWIG" "$PB" "$NIXP"; do
  [ -n "$t" ] || { echo "NO ANSWER: a tool did not resolve"; exit 2; }
done

D=$(mktemp -d -t nn-readerwiring-XXXXXX)
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D"' EXIT

# drive <driver> <taskbin> <build dir> <target> > json
drive() {
  ( cd "$3"
    PATH="$1/bin:$2/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin:$MESON/bin:$SWIG/bin:$PB/bin" \
    NIX_NINJA_TASK="$2/bin/nix-ninja-task" \
    NIX_CONFIG="experimental-features = $FEAT" \
    NIX_NINJA_IMPLICIT_INPUTS_LIMIT="${DRIVE_IMPLICIT_LIMIT:-0}" \
    "$1/bin/nix-ninja" -t drv "$4" 2>"$3/drive.err"
  )
}

# carries <json> <substring>
carries() { command grep -c "$2" "$1" 2>/dev/null || true; }

# carries_inputs <json> <substring>: matches inside NIX_NINJA_INPUTS only.
# `carries` greps the whole derivation including `args`, so a subject the
# command itself names (a copy source IS argv) reads carried on both sides
# and the pin control can never fail. Scoped counting is the fix the arm-23
# pin failure paid for.
carries_inputs() { python3 -c "
import json,sys
try: d=json.load(open('$1'))
except Exception: print(0); sys.exit()
print(sum(v.get('env',{}).get('NIX_NINJA_INPUTS','').count('$2') for v in d.values()))"; }

# carries_outputs <json> <substring>: matches inside NIX_NINJA_OUTPUTS only,
# for the same reason: the doxygen edge's own command names its html tree
# (`cmake -E make_directory`), so a whole-derivation grep reads it carried
# before any reader runs.
carries_outputs() { python3 -c "
import json,sys
try: d=json.load(open('$1'))
except Exception: print(0); sys.exit()
print(sum(v.get('env',{}).get('NIX_NINJA_OUTPUTS','').count('$2') for v in d.values()))"; }

# ---- ARM 1: libnvme's swig `%include`, resolved by swig against the CWD ----
S1=$D/swig; mkdir -p "$S1/build" "$S1/libnvme" "$S1/src/nvme"
printf 'int types_seed(void);\n' > "$S1/src/nvme/types.h"
printf '%%module nvme\n%%include "../src/nvme/types.h"\n' > "$S1/libnvme/nvme.i"
# The edge declares ONLY the .i. types.h exists on disk and is declared by no
# edge, which is the class: swig resolves the %include itself.
printf 'rule swig\n  command = swig -o $out $in\nbuild nvme_wrap.c: swig ../libnvme/nvme.i\n' \
  > "$S1/build/build.ninja"
drive "$NN" "$TASK" "$S1/build" nvme_wrap.c > "$D/swig.json" 2>"$D/swig.rc.err" || {
  echo "NO ANSWER: the swig drive did not emit a derivation"; tail -3 "$S1/build/drive.err"; exit 2; }
S_HERE=$(carries "$D/swig.json" 'types\.h')
S_DECL=$(carries "$D/swig.json" 'nvme\.i')

# ---- ARM 26: qtwebengine's flatc `include`, resolved through -I ------------
# MEASURED 2026-10-20 on qtwebengine 6.11.2 (task
# `nclyj94lf5afqs4byc9cszbdhvvnqm99-ninja-build.drv`): the flatc edge declares
# only `indexed_ruleset.fbs`, which includes
# `components/url_pattern_index/flat/url_pattern_index.fbs` through the
# command's `-I` root, and the run died `unable to load include file`. Same
# family as the swig `%include` and protoc `import` readers. The tool is a
# stub that exits 0: the drive only parses argv, and production runs flatc
# behind `gn_run_binary.py host/flatc`, which the trigger matches by
# basename. The `-I` uses the separated spelling production emits.
S26=$D/flatc; mkdir -p "$S26/build/gen" "$S26/build/tools" "$S26/chromium/x" "$S26/chromium/common"
printf '#!/bin/sh\nexit 0\n' > "$S26/build/tools/flatc"; chmod +x "$S26/build/tools/flatc"
printf 'include "common/inc.fbs";\ntable T { a: int; }\nroot_type T;\n' > "$S26/chromium/x/top.fbs"
printf 'include "nested.fbs";\ntable I { b: int; }\n' > "$S26/chromium/common/inc.fbs"
printf 'table N { c: int; }\n' > "$S26/chromium/common/nested.fbs"
# THE -I ROOT MUST BE OVER DIR_UPLOAD_CAP AND HOLD A PYTHON PACKAGE, as
# chromium's is. The dir-arg route carries a small root whole, so a
# three-file root reads carried at the pin and the control cannot fail
# (measured: pin inc=1 on the first version of this arm). Over the cap with
# a package, that route uploads only the package, which is what production
# printed: `dir arg .../src/3rdparty/chromium exceeds 1024 files; uploaded
# only its python packages (327 files)`.
mkdir -p "$S26/chromium/pkg" "$S26/chromium/filler"
: > "$S26/chromium/pkg/__init__.py"
( cd "$S26/chromium/filler" && seq 1 1100 | xargs touch )
cat > "$S26/build/build.ninja" <<EOF
rule flatc
  command = $S26/build/tools/flatc -c --keep-prefix -o gen -I ../chromium ../chromium/x/top.fbs
build gen/top_generated.h: flatc ../chromium/x/top.fbs
EOF
drive "$NN" "$TASK" "$S26/build" gen/top_generated.h > "$D/flatc.json" 2>"$D/flatc.rc.err" || {
  echo "NO ANSWER: the flatc drive did not emit a derivation"; tail -3 "$S26/build/drive.err"; exit 2; }
F_HERE=$(carries_inputs "$D/flatc.json" 'common/inc.fbs')
F_DECL=$(carries_inputs "$D/flatc.json" 'chromium/x/top.fbs')
# THE SECOND LEVEL, carried only by a closure: flatc parses an included
# schema as a source of its own, so `nested.fbs` is opened as surely as
# `inc.fbs`. The pin has no flatc reader at all, so its 0 isolates the
# closure rather than the reader.
F_NESTED=$(carries_inputs "$D/flatc.json" 'common/nested.fbs')

# ---- ARM 2: colord's msgfmt ITS lookup through --datadirs --------------
S2=$D/its; mkdir -p "$S2/build" "$S2/data/profiles/its"
printf '<locatingRules>\n  <locatingRule pattern="*.iccprofile.xml">\n    <documentRule localName="profile" target="colord.its"/>\n  </locatingRule>\n</locatingRules>\n' \
  > "$S2/data/profiles/its/colord.loc"
printf '<its/>\n' > "$S2/data/profiles/its/colord.its"
# The REAL command form meson emits, and the .its name appears in NO meson file:
# only the locating rule names it.
printf 'rule fmt\n  command = meson --internal msgfmthelper --datadirs=../data/profiles in out x\nbuild out.xml: fmt ../data/profiles/its/colord.loc\n' \
  > "$S2/build/build.ninja"
drive "$NN" "$TASK" "$S2/build" out.xml > "$D/its.json" 2>"$D/its.rc.err" || {
  echo "NO ANSWER: the ITS drive did not emit a derivation"; tail -3 "$S2/build/drive.err"; exit 2; }
I_HERE=$(carries "$D/its.json" 'colord\.its')

# ---- ARM 3: upb's protoc `import` closure, resolved through -I ----------
# The edge declares ONLY the argv proto; its imports are siblings the command
# resolves through `-I .`, exactly upb's failing edges (134 tasks, two desktop
# rounds at pin 348e2cf). Without the reader the derivation carries the argv
# proto and neither import.
#
# ONE ARM PER IMPORT QUALIFIER, and the qualifier is the arm that matters:
# protobuf 36.2's own tree carries `import "` 247, `import option "` 33 and
# `import public "` 27. The first version of this arm used only the plain and
# `public` forms, so it passed while `import option` - the `edition = "2024"`
# spelling that failed 162 tasks on KDE at pin 16a3e01 - was dropped in
# silence. A fixture that exercises two of three spellings cannot see the
# third, which is the fixture-shape lesson this corpus keeps paying for.
S3=$D/proto; mkdir -p "$S3/build/upb/util" "$S3/build/upb/test"
printf 'syntax = "proto2";\nimport public "upb/util/def_to_proto_public_import_test.proto";\nimport "upb/util/def_to_proto_regular_import_test.proto";\nimport option "upb/test/custom_options.proto";\n' \
  > "$S3/build/upb/util/def_to_proto_test.proto"
printf 'syntax = "proto2";\n' > "$S3/build/upb/util/def_to_proto_public_import_test.proto"
printf 'syntax = "proto2";\n' > "$S3/build/upb/util/def_to_proto_regular_import_test.proto"
printf 'syntax = "proto2";\n' > "$S3/build/upb/test/custom_options.proto"
# The real command shape from the failing derivation (its `cd` is ABSOLUTE
# and so lands on the build dir itself; a relative `cd build` here would
# re-root the reader one level too deep, the known rebase trap), plugin
# flag, one -I root, one positional proto.
printf 'rule protoc\n  command = protoc --upbdefs_out :. --plugin=protoc-gen-upbdefs=protoc-gen-upbdefs -I . upb/util/def_to_proto_test.proto\nbuild upb/util/def_to_proto_test.upbdefs.h: protoc upb/util/def_to_proto_test.proto\n' \
  > "$S3/build/build.ninja"
drive "$NN" "$TASK" "$S3/build" upb/util/def_to_proto_test.upbdefs.h > "$D/proto.json" 2>"$D/proto.rc.err" || {
  echo "NO ANSWER: the protoc drive did not emit a derivation"; tail -3 "$S3/build/drive.err"; exit 2; }
P_PUB=$(carries "$D/proto.json" 'def_to_proto_public_import_test\.proto')
P_REG=$(carries "$D/proto.json" 'def_to_proto_regular_import_test\.proto')
P_OPT=$(carries "$D/proto.json" 'upb/test/custom_options\.proto')

# ---- ARM 4: an ExternalProject step's ninja on PATH, and its BINARY_DIR ----
# The two ExternalProject arms (34038d2, with the gates at e93c9da and 425778a),
# at -t drv level so the corpus can guard them in seconds.
# The B witness (external-project-configure-b.sh) is the number for the package,
# but it is minutes and excluded; a regression to either arm would otherwise go
# unguarded between drives. Both arms are pure cmdline/input-set logic, which is
# exactly what -t drv exercises.
#
# NINJA MUST BE AN INPUT for the PATH arm to fire (ninja_input asks the step's
# OWN inputs), and it arrives the way it does in the real drv - through
# XDG_DATA_DIRS naming ninja's share/. CMake is on PATH here so cmdline_binary
# resolves and the step is not refused first.
S4=$D/ep; mkdir -p "$S4/build/deps/src/dep"
printf 'cmake_minimum_required(VERSION 3.20)\n' > "$S4/build/deps/src/dep/CMakeLists.txt"
# The real nghttp2 configure shape: leading cd into the binary dir it writes,
# -GNinja, the separated -S and -B, and the stamp touch.
printf 'rule cfg\n  command = cd . && %s -GNinja -S deps/src/dep -B deps/build/dep && %s -E touch dep-prefix/src/dep-stamp/dep-configure\nbuild dep-prefix/src/dep-stamp/dep-configure: cfg\n' "$CMAKE_BIN" "$CMAKE_BIN" \
  > "$S4/build/build.ninja"
mkdir -p "$S4/build/dep-prefix/src/dep-stamp"
# Drive with ninja's share on XDG_DATA_DIRS, which is how the real drv carries it.
NINJA_STORE=$(store_find '*-ninja-1.13.2')
if [ -z "$NINJA_STORE" ]; then
  echo "NO ANSWER: no ninja-1.13.2 in the store, so the PATH arm cannot be exercised"; exit 2
fi
( cd "$S4/build"
  PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin:$CMAKE_BIN_DIR" \
  XDG_DATA_DIRS="$NINJA_STORE/share" \
  NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
  NIX_CONFIG="experimental-features = $FEAT" \
  NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
  "$NN/bin/nix-ninja" -t drv dep-prefix/src/dep-stamp/dep-configure > "$D/ep.json" 2>"$S4/build/drive.err"
) || { echo "NO ANSWER: the ExternalProject drive did not emit a derivation"; tail -3 "$S4/build/drive.err"; exit 2; }
E_NINJA=$(python3 -c "
import json,sys
try: d=json.load(open('$D/ep.json'))
except Exception: print(0); sys.exit()
print(int(any('ninja-1.13' in p for v in d.values() for p in v['env'].get('PATH','').split(':'))))")
E_BINDIR=$(python3 -c "
import json,sys
try: d=json.load(open('$D/ep.json'))
except Exception: print(0); sys.exit()
print(int(any(l.split(':')[1]=='deps/build/dep' for v in d.values() for l in v['env'].get('NIX_NINJA_OUTPUTS','').split(chr(10)) if l)))")

# ---- ARM 5: a reader reached by EVERY task must be gated on its subject ----
# THE CLASS THAT SHIPPED BROKEN TWICE. `external_project_binary_dir` is called
# from `undeclared_outputs`, which every task with a cmdline reaches, and its
# first two versions matched on the FLAG PAIR (`-S` with `-B`) instead of on the
# step - so it fired on CMake's REGENERATE edge, which CMake writes into every
# CMake+Ninja build.ninja:
#     cmake --regenerate-during-build -S<src> -B<build>
# and declared the binary dir as an undeclared OUTPUT TREE of a task that is not
# an ExternalProject step. Reproduced 2026-10-12 against e93c9da: the fixture
# below emitted ['build.ninja', 'sub'] where HEAD emits ['build.ninja'].
#
# WHAT THIS ARM ASSERTS: an edge whose output is NOT step-shaped, carrying the
# real regenerate pair, declares NO tree. A regression to an ungated reader puts
# `sub` back in NIX_NINJA_OUTPUTS and this FAILS. `-B` points UNDER the build
# dir deliberately: a destination outside it is dropped by relative_from, which
# is why a fixture with an absolute -B elsewhere cannot see this class.
S5=$D/regen; mkdir -p "$S5/build/CMakeFiles"
printf 'stamp\n' > "$S5/build/CMakeFiles/foo.stamp"
printf 'rule RERUN_CMAKE\n  command = cd . && %s --regenerate-during-build -S%s/build -B%s/build/sub\n  pool = console\nbuild build.ninja: RERUN_CMAKE | CMakeFiles/foo.stamp\n' "$CMAKE_BIN" "$S5" "$S5" \
  > "$S5/build/build.ninja"
( cd "$S5/build"
  PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin:$CMAKE_BIN_DIR" \
  NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
  NIX_CONFIG="experimental-features = $FEAT" \
  NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
  "$NN/bin/nix-ninja" -t drv build.ninja > "$D/regen.json" 2>"$S5/build/drive.err"
) || { echo "NO ANSWER: the REGENERATE drive did not emit a derivation"; tail -3 "$S5/build/drive.err"; exit 2; }
E_REGEN=$(python3 -c "
import json,sys
try: d=json.load(open('$D/regen.json'))
except Exception: print(-1); sys.exit()
outs=[l.split(':')[1] for v in d.values() for l in v['env'].get('NIX_NINJA_OUTPUTS','').split(chr(10)) if l]
print(int('sub' in outs))")

# ---- ARM 6: a `cd` ABOVE the build dir, which the rewrite DROPS ----------
# THE ARM THAT WOULD HAVE CAUGHT THE FIRST VERSION OF THE glib-mkenums READER,
# and the reason it exists: that version resolved the command's names against
# the BUILD DIR, because `rewrite_cmdline` DELETES a `cd` prologue whose target
# is above the build dir (its "A CD TARGET ABOVE THE BUILD DIR" arm) - so the
# `cd` the reader saw had already been removed and its cwd fell back to the
# build dir. The reader looked in the wrong directory and carried nothing, which
# the unit test could not see because it called the resolver DIRECTLY with the
# cwd it had chosen. Measured 2026-10-14: the drive on ayatana-ido's own
# build.ninja emitted 15 inputs and no template, while the file sat at `../src`.
#
# THE GEOMETRY IS THE POINT: the template and the headers live in a directory
# that is a SIBLING of the build dir, reached by the command's own `cd`, and the
# edge declares none of them. A fixture that put them UNDER the build dir would
# pass against the broken reader and model nothing.
S6=$D/mkenums; mkdir -p "$S6/src" "$S6/build"
printf 'TEMPLATE
' > "$S6/src/gen.h.template"
printf '#define A 1\n' > "$S6/src/a.h"
printf 'rule mkenums\n  command = cd %s/src && %s --template gen.h.template a.h --output=%s/build/gen.h\nbuild gen.h: mkenums\n' \
  "$S6" "$MKENUMS_BIN" "$S6" > "$S6/build/build.ninja"
printf 'int main(void){return 0;}\n' > /dev/null
( cd "$S6/build"
  PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin" \
  NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
  NIX_CONFIG="experimental-features = $FEAT" \
  NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
  "$NN/bin/nix-ninja" -t drv gen.h > "$D/mkenums.json" 2>"$S6/build/drive.err"
) || { echo "NO ANSWER: the glib-mkenums drive did not emit a derivation"; tail -3 "$S6/build/drive.err"; exit 2; }
M_TMPL=$(carries "$D/mkenums.json" 'gen\.h\.template')
M_HDR=$(carries "$D/mkenums.json" 'src/a\.h')

# ---- ARM 7: a scanner that shells out to the compiler UNNAMED -----------
# THE THIRD SHAPE of "execs the compiler without naming it", and the two
# existing arms are both silent on it: the NAME test sees no bare toolchain
# (the scanner is named by absolute store path), and the OUTPUT-shape arm
# exists for generators that write an object (`dtrace -G`), while this one
# writes a `.gir`.
#
# MEASURED 2026-10-16 on libayatana-appindicator 0.5.92, task
# `cnayivpcpxk9j6dgd7abvjndj7cvh5gf`: g-ir-scanner resolved its sources and
# ran, then died `FileNotFoundError: [Errno 2] 'gcc'` through
# `giscanner/ccompiler.py` -> distutils `UnixCCompiler` -> `spawn.py`, with
# ten PATH entries and no compiler among them.
#
# WHAT THIS SCORES IS THE EMITTED PATH, not the input list: the fix carries
# the compiler CLOSURE and pushes its `bin` onto the task's PATH, so a
# fixture that only counted inputs would read zero on both sides. The
# scanner here is a stub that prints its own PATH, because the arm is about
# what the DRIVER emits and a real scanner would need the whole
# introspection toolchain to run.
S7=$D/gircc; mkdir -p "$S7/src" "$S7/build"
printf 'int x;\n' > "$S7/src/app.c"
# The stub stands in for g-ir-scanner and must be reachable by the ABSOLUTE
# path the command spells, which is the property that made the name test
# silent. It writes the declared output so the drive has a shape to emit.
printf '#!/bin/sh\necho "$PATH" > "$(dirname "$0")/seen_path.txt"\n: > "$1"\n' > "$S7/g-ir-scanner"
chmod +x "$S7/g-ir-scanner"
printf 'rule gir\n  command = cd %s/src && %s/g-ir-scanner %s/build/out.gir app.c\nbuild out.gir: gir\n' \
  "$S7" "$S7" "$S7" > "$S7/build/build.ninja"
( cd "$S7/build"
  PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin" \
  NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
  NIX_CONFIG="experimental-features = $FEAT" \
  NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
  "$NN/bin/nix-ninja" -t drv out.gir > "$D/gircc.json" 2>"$S7/build/drive.err"
) || { echo "NO ANSWER: the scanner-compiler drive did not emit a derivation"; tail -3 "$S7/build/drive.err"; exit 2; }
# The compiler must reach the task BOTH as an input and as a PATH entry, and
# THE TWO LIVE IN DIFFERENT FIELDS: `task.rs` inserts the compiler as an
# `Opaque` input, which lands in `inputs.srcs`, while `inputs.drvs` holds the
# DYNAMIC derivation inputs and is EMPTY here. Reading `drvs` alone returned 0
# against a derivation that carried the wrapper, which is the three-input-lists
# trap CLAUDE.md records. Both fields are read, and the PATH entry is asserted
# too, because a fix that supplied one without the other would still fail.
G_CC=$(python3 -c "
import json
try: d=json.load(open('$D/gircc.json'))
except Exception: print(-1); sys.exit()
n=0
for v in d.values():
    ins=v.get('inputs',{})
    allins=' '.join(list(ins.get('srcs',[]))+list(ins.get('drvs',{}).keys()))
    onpath='gcc-wrapper' in v.get('env',{}).get('PATH','')
    if ('gcc-wrapper' in allins or 'gcc-16' in allins) and onpath: n+=1
print(n)")

# ---- ARM 8: the headers a scanned gir source INCLUDES -------------------
# g-ir-scanner preprocesses every header it is handed, so their includes are
# read too and no edge declares them. MEASURED 2026-10-16 on gnome-desktop
# 44.5's GnomeBG gir (task `p0iam8fhbzz7rbxa2x79195jga9hw3yv`): the filelist
# names `gnome-bg/gnome-bg.h`, which includes
# `<libgnome-desktop/gnome-desktop-thumbnail.h>` through `-I.../libgnome-desktop/..`,
# and the task died on it. The included header sits in a SIBLING directory of
# the listed one, reached only through the -I chain, and the build dir is a
# child of the source tree, which is the geometry that matters.
S8=$D/girinc; mkdir -p "$S8/src/lib/bg" "$S8/build"
printf '#include <lib/thumb.h>\n' > "$S8/src/lib/bg/bg.h"
printf '#define T 1\n' > "$S8/src/lib/thumb.h"
printf '%s/src/lib/bg/bg.h\n' "$S8" > "$S8/build/fl"
printf 'rule gir\n  command = %s/g-ir-scanner --filelist=%s/build/fl --output %s/build/out.gir -I%s/src/lib/.. --cflags-begin -I%s/src/lib/.. --cflags-end\nbuild out.gir: gir\n' \
  "$S7" "$S8" "$S8" "$S8" "$S8" > "$S8/build/build.ninja"
drive "$NN" "$TASK" "$S8/build" out.gir > "$D/girinc.json" \
  || { echo "NO ANSWER: the gir header-closure drive did not emit a derivation"; tail -3 "$S8/build/drive.err"; exit 2; }
H_THUMB=$(carries "$D/girinc.json" 'thumb\.h')
H_LISTED=$(carries "$D/girinc.json" 'bg\.h')

# ---- ARM 9: an assembler's `%include`, resolved through its -I roots -----
# MEASURED 2026-10-16 on gst-plugins-good 1.28.7 (task
# `nid4161whsc19m3wdw8b27y88jrbqzg9`): `nasm -I../gst/deinterlace/x86/ ...
# ../gst/deinterlace/x86/yadif.asm` died `unable to open include file
# 'x86inc.asm'`; the edge declares the .asm alone. The geometry is the real
# one: sources ABOVE the build dir, the include reached through a -I root.
NASM=$(px nasm)
S9=$D/nasminc; mkdir -p "$S9/src/x86" "$S9/build"
printf '%%include "x86inc.asm"\n' > "$S9/src/x86/yadif.asm"
printf '; nothing\n' > "$S9/src/x86/x86inc.asm"
printf 'rule asm\n  command = %s/bin/nasm -I../src/x86/ -f elf64 -o yadif.o ../src/x86/yadif.asm\nbuild yadif.o: asm ../src/x86/yadif.asm\n' \
  "$NASM" > "$S9/build/build.ninja"
drive "$NN" "$TASK" "$S9/build" yadif.o > "$D/nasm.json" \
  || { echo "NO ANSWER: the nasm drive did not emit a derivation"; tail -3 "$S9/build/drive.err"; exit 2; }
N_INC=$(carries "$D/nasm.json" 'x86inc\.asm')

# ---- ARM 10: a library's SONAME LINK made by a sibling edge -------------
# MEASURED 2026-10-16 on onetbb 2022.3.0 (task `06gw256r1a7k8v0h6khxqdr906riaysn`):
# the test executable's link carried `libtbbmalloc.so.2.17` but not
# `libtbbmalloc.so.2`, which a library it links names in DT_NEEDED and which
# only the CMAKE_SYMLINK_LIBRARY edge declares; ld died `undefined reference
# to scalable_malloc`. The edge shapes are onetbb's own build.ninja lines:
# the test library order-only on the namelink, the executable on the test
# library alone.
S10=$D/soname; mkdir -p "$S10/build/lib"
cat > "$S10/build/build.ninja" <<NINJA
rule run
  command = \$cmd
build lib/libm.so.2.17: run
  cmd = touch lib/libm.so.2.17
build lib/libm.so.2 lib/libm.so: run lib/libm.so.2.17
  cmd = cd $S10/build/lib && $CMAKE_BIN -E cmake_symlink_library libm.so.2.17 libm.so.2 libm.so
build lib/libt.so: run | lib/libm.so.2.17 || lib/libm.so
  cmd = touch lib/libt.so
build lib/t: run | lib/libt.so || lib/libt.so
  cmd = touch lib/t
build stamp: run || lib/libm.so
  cmd = touch stamp
build lib/d: run | lib/libm.so.2.17 || stamp
  cmd = touch lib/d
NINJA
( cd "$S10/build"
  PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin:$CMAKE_BIN_DIR" \
  NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
  NIX_CONFIG="experimental-features = $FEAT" \
  NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
  "$NN/bin/nix-ninja" -t drv lib/t > "$D/soname.json" 2>"$S10/build/drive.err"
) || { echo "NO ANSWER: the soname-link drive did not emit a derivation"; tail -3 "$S10/build/drive.err"; exit 2; }
L_REAL=$(carries "$D/soname.json" 'libm\.so\.2\.17')
L_SONAME=$(carries "$D/soname.json" 'lib/libm\.so\.2[^.]')
# THE BANKED SHAPE: a task handed the library DIRECTLY names it by path and
# must NOT gain the soname link, or every such link already built re-keys.
# The stamp makes the symlink edge a dependency, so it is REGISTERED before
# `d` as in a whole-graph build; without it the map is empty in every
# version and this control cannot fail.
( cd "$S10/build"
  PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin:$CMAKE_BIN_DIR" \
  NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
  NIX_CONFIG="experimental-features = $FEAT" \
  NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
  "$NN/bin/nix-ninja" -t drv lib/d > "$D/soname.direct.json" 2>"$S10/build/drive.err"
) || { echo "NO ANSWER: the direct soname drive did not emit a derivation"; exit 2; }
L_DIRECT=$(carries "$D/soname.direct.json" 'lib/libm\.so\.2[^.]')
L_DIRECT_REAL=$(carries "$D/soname.direct.json" 'libm\.so\.2\.17')

# ---- ARM 11: vapigen's `<metadatadir>/<Namespace>.metadata` -------------
# MEASURED 2026-10-16 on packagekit (task `rh67f8sz356b7b9m8h096iwjyw3c6af4`):
# `vapigen --metadatadir=/build/source/lib .../PackageKitGlib-1.0.gir` never
# saw `PackageKitGlib-1.0.metadata`, whose `ErrorEnum`/`DistroUpgradeEnum`
# renames resolve the two name clashes, and rejected both constructors. The
# file is opened by NAME; no argument spells it. The `-1.0` is in the fixture
# on purpose: the reader's first version took `.0` for the extension.
VALA=$(px vala)
[ -x "$VALA/bin/vapigen" ] || { echo "NO ANSWER: vapigen did not resolve, so arm 11 cannot run"; exit 2; }
S11=$D/vapigen; mkdir -p "$S11/src/lib" "$S11/src/build/lib"
printf 'ErrorEnum name="ErrorEnum"\n' > "$S11/src/lib/Pk-1.0.metadata"
printf '<repository/>\n' > "$S11/src/build/lib/Pk-1.0.gir"
printf 'rule vapi\n  command = %s/bin/vapigen --library=pk --directory=%s/src/build/lib --metadatadir=%s/src/lib %s/src/build/lib/Pk-1.0.gir\nbuild lib/pk.vapi: vapi lib/Pk-1.0.gir\n' \
  "$VALA" "$S11" "$S11" "$S11" > "$S11/src/build/build.ninja"
drive "$NN" "$TASK" "$S11/src/build" lib/pk.vapi > "$D/vapigen.json" \
  || { echo "NO ANSWER: the vapigen drive did not emit a derivation"; tail -3 "$S11/src/build/drive.err"; exit 2; }
V_MD=$(carries "$D/vapigen.json" 'Pk-1\.0\.metadata')
V_GIR=$(carries "$D/vapigen.json" 'Pk-1\.0\.gir')

# ---- ARM 12: a generated SOURCE the TU includes, reached through a phony -
# MEASURED 2026-10-16 on qtbase 6.11.2 (tasks `yxic3b85`, `47wvhj14`): the
# moc edge produces `moc_qobject.cpp` and `moc_qobject.cpp.json` together and
# reaches the compile only through `cmake_object_order_depends_target_Core`;
# the `.json` was carried and the `.cpp` dropped by the compile-artifact
# filter, and `#include "moc_qobject.cpp"` died. The sibling `moc_other.cpp`
# the TU does NOT include must stay dropped, or perfetto's `.gen.cc` world
# rides into every TU again - that is this arm's own control.
S12=$D/mocinc; mkdir -p "$S12/build" "$S12/src"
printf '#include "moc_qobject.cpp"\nint main(void){return 0;}\n' > "$S12/src/qobject.cpp"
cat > "$S12/build/build.ninja" <<EOF
rule gen
  command = touch \$out
rule cxx
  command = gcc -I. -c \$in -o \$out
  deps = gcc
  depfile = \$out.d
build moc_qobject.cpp moc_qobject.cpp.json: gen
build moc_other.cpp: gen
build cmake_object_order_depends_target_T: phony moc_qobject.cpp.json moc_other.cpp
build qobject.o: cxx ../src/qobject.cpp || cmake_object_order_depends_target_T
EOF
drive "$NN" "$TASK" "$S12/build" qobject.o > "$D/mocinc.json" \
  || { echo "NO ANSWER: the moc-include drive did not emit a derivation"; tail -3 "$S12/build/drive.err"; exit 2; }
M_INC=$(carries "$D/mocinc.json" 'moc_qobject\.cpp[^.]')
M_JSON=$(carries "$D/mocinc.json" 'moc_qobject\.cpp\.json')
M_OTHER=$(carries "$D/mocinc.json" 'moc_other\.cpp')
echo "mocinc moc_qobject.cpp here=$M_INC json=$M_JSON other=$M_OTHER"

# ---- ARM 13: GETTEXTDATADIRS reaches a msgfmt task and NO other ------------
# MEASURED 2026-10-16 on colord 1.4.8 (`igix4pvq`) and malcontent
# (`i7f5q41d`): msgfmt died `cannot locate ITS rules for <x>.policy.in` with
# polkit's `share/gettext/its/polkit.{its,loc}` in `srcs` and the variable
# gettext's env hook exports ABSENT from the task's 13 env keys. The forward
# is COMMAND-SCOPED: a compile or a swig task in the same build must not
# gain the variable or its store paths, or every banked task in every
# gettext-bearing build re-keys. A real store path, added here, so a collect
# cannot turn this arm into NO ANSWER.
mkdir -p "$D/gtd/its"; printf '<its/>\n' > "$D/gtd/its/polkit.its"
GTD_STORE=$(nix --extra-experimental-features "$FEAT" store add --name gettext-data "$D/gtd")
[ -n "$GTD_STORE" ] || { echo "NO ANSWER: nix store add produced no path for arm 13"; exit 2; }
GETTEXTDATADIRS="$GTD_STORE" drive "$NN" "$TASK" "$S2/build" out.xml > "$D/gtd.json" \
  || { echo "NO ANSWER: the ITS drive with GETTEXTDATADIRS set did not emit a derivation"; exit 2; }
GETTEXTDATADIRS="$GTD_STORE" drive "$NN" "$TASK" "$S1/build" nvme_wrap.c > "$D/gtd.ctl.json" \
  || { echo "NO ANSWER: the swig drive with GETTEXTDATADIRS set did not emit a derivation"; exit 2; }
G_ENV=$(carries "$D/gtd.json" '"GETTEXTDATADIRS"')
G_SRC=$(carries "$D/gtd.json" "${GTD_STORE##*/}")
G_CTL=$(carries "$D/gtd.ctl.json" 'GETTEXTDATADIRS\|'"${GTD_STORE##*/}")
echo "gettextdatadirs msgfmt env=$G_ENV src=$G_SRC  swig(control)=$G_CTL"

# ---- ARM 14: the uninstalled gir a typelib's gir INCLUDES -----------------
# MEASURED 2026-10-16 on gnome-desktop 44.5 (`ll5hqs1y`): `g-ir-compiler
# GnomeBG-4.0.gir --includedir=<build>/libgnome-desktop` died `Could not find
# GIR file 'GnomeDesktop-4.0.gir'`; the typelib edge declares its own gir
# alone (14 inputs), and the include is spelled only on the SCANNER's edge as
# `--include-uninstalled=<path>`. MESON SPELLS IT RELATIVE TO THE BUILD DIR
# (the real scanner drv `j6lplawg`: `libgnome-desktop/GnomeDesktop-4.0.gir`);
# the first fixture spelled it absolute, the fix took only that spelling, and
# the round of 2026-10-16 failed on the real one with the fix mounted. B is
# the production spelling (subject), C the absolute one (kept covered).
# Commands are never run under -t drv; the programs never run.
S14=$D/girinc2; mkdir -p "$S14/build/lib" "$S14/src"
printf '#!/bin/sh\n: > "$1"\n' > "$S14/g-ir-scanner"; cp "$S14/g-ir-scanner" "$S14/g-ir-compiler"
chmod +x "$S14/g-ir-scanner" "$S14/g-ir-compiler"
cat > "$S14/build/build.ninja" <<EOF
rule scan
  command = $S14/g-ir-scanner --namespace=\$ns --output \$out \$incl
rule tc
  command = $S14/g-ir-compiler \$in --output \$out --includedir=$S14/build/lib
build lib/B-1.0.gir: scan
  ns = B
  incl =
build lib/C-1.0.gir: scan
  ns = C
  incl =
build lib/A-1.0.gir: scan | lib/B-1.0.gir lib/C-1.0.gir
  ns = A
  incl = --include-uninstalled=lib/B-1.0.gir --include-uninstalled=$S14/build/lib/C-1.0.gir
build lib/A-1.0.typelib: tc lib/A-1.0.gir
EOF
drive "$NN" "$TASK" "$S14/build" lib/A-1.0.typelib > "$D/girinc2.json" \
  || { echo "NO ANSWER: the typelib drive did not emit a derivation"; tail -3 "$S14/build/drive.err"; exit 2; }
T_A=$(carries "$D/girinc2.json" 'lib/A-1\.0\.gir')
T_B=$(carries "$D/girinc2.json" 'lib/B-1\.0\.gir')
T_C=$(carries "$D/girinc2.json" 'lib/C-1\.0\.gir')
echo "typelib includes own gir=$T_A included gir relative=$T_B absolute=$T_C"

# ---- ARM 15: a directory compiled in by a define, opened by the binary a
# link edge RUNS (gtest discovery) ------------------------------------------
# MEASURED 2026-10-16 on libjxl (`3ij3xhpl`): `-DTEST_DATA_PATH=<src>/testdata`
# sits on test_utils.cc's compile edge, rides the object into
# libjxl_testlib-internal.a, and gain_map_test's link edge runs the binary
# (GoogleTestAddTests.cmake POST_BUILD), which opens the directory and traps.
# The define must follow object -> archive -> link, and ONLY the edge that
# runs the binary carries the directory: the compile is the control. THE
# ARCHIVE IS AN IMPLICIT INPUT OF THE LINK, as CMake writes it (`| lib/x.a`
# with the name in LINK_LIBRARIES, real edge `az5f2s1p`); the first fixture
# made it explicit, the fix walked explicit inputs only, and the round of
# 2026-10-16 failed with that fix mounted.
S15=$D/pathdef; mkdir -p "$S15/build/lib" "$S15/build/tests" "$S15/src/testdata"
printf 'icc\n' > "$S15/src/testdata/scRGB-v2.icc"
printf 'int seed(void){return 0;}\n' > "$S15/src/test_utils.cc"
printf 'int main(void){return 0;}\n' > "$S15/src/t.cc"
GTA=$(ls "$CM"/share/cmake-*/Modules/GoogleTestAddTests.cmake | head -1)
[ -f "$GTA" ] || { echo "NO ANSWER: GoogleTestAddTests.cmake not in cmake's share, so arm 15 cannot run"; exit 2; }
# CMake 4 writes `<test>_<hash>_discovery.cmake` at generate time and the
# link runs THAT with -P; its first line includes the module (real script
# `gain_map_test_9cf322f2_discovery.cmake`). The module's name is never on
# the command line; the first fixture put it there and the trigger matched
# it, so production never fired.
printf 'include("%s")\ngtest_discover_tests_impl(\n  TEST_TARGET [==[t]==]\n  TEST_EXECUTABLE [==[%s/build/tests/t]==]\n)\n' "$GTA" "$S15" > "$S15/build/tests/t_deadbeef_discovery.cmake"
cat > "$S15/build/build.ninja" <<EOF
rule cxxdef
  command = g++ -DTEST_DATA_PATH=\\"$S15/src/testdata\\" -c \$in -o \$out
  deps = gcc
  depfile = \$out.d
rule cxx
  command = g++ -c \$in -o \$out
  deps = gcc
  depfile = \$out.d
rule ar
  command = $CC/bin/ar qc \$out \$in
rule link
  command = g++ \$in -o \$out \$LINK_LIBRARIES && cd $S15/build/tests && $CMAKE_BIN -P $S15/build/tests/t_deadbeef_discovery.cmake --
build lib/test_utils.o: cxxdef ../src/test_utils.cc
build lib/libtestlib.a: ar lib/test_utils.o
build tests/t.o: cxx ../src/t.cc
build tests/t: link tests/t.o | lib/libtestlib.a || lib/libtestlib.a
  LINK_LIBRARIES = lib/libtestlib.a
EOF
drive "$NN" "$TASK" "$S15/build" tests/t > "$D/pathdef.json" \
  || { echo "NO ANSWER: the gtest-link drive did not emit a derivation"; tail -3 "$S15/build/drive.err"; exit 2; }
D_LINK=$(carries "$D/pathdef.json" 'testdata/scRGB-v2\.icc')
drive "$NN" "$TASK" "$S15/build" lib/test_utils.o > "$D/pathdef.ctl.json" \
  || { echo "NO ANSWER: the define compile drive did not emit a derivation"; exit 2; }
D_CTL=$(carries "$D/pathdef.ctl.json" 'testdata/scRGB-v2\.icc')
echo "pathdef link carries testdata=$D_LINK  compile(control)=$D_CTL"

# ---- ARM 16: the content files a gi-docgen TOML names ----------------------
# MEASURED 2026-10-17 on libgweather 4.6.0 (task `zdrin8s7`, ninja route,
# driver `wmp9qgyj`): `gi-docgen generate --config doc/libgweather.toml
# --content-dir=<src>/doc <gir>` died `Content file libgweather-logo.svg not
# found in any content directory`. meson declares the TOML and the pages
# listed as `depend_files`; the TOML's `content_images` and `urlmap_file`
# are read by the tool alone. Argv copied from the drv's `args`; the page is
# declared (floor), the image and the urlmap are the subject. The TOML sits
# in the BUILD dir (configure_file), the content dir in the SOURCE tree.
S16=$D/gidoc; mkdir -p "$S16/build/doc" "$S16/build/lib" "$S16/doc"
printf '#!/bin/sh\nmkdir -p "$3"\n' > "$S16/gi-docgen"; chmod +x "$S16/gi-docgen"
: > "$S16/build/lib/X-1.0.gir"
printf '# page\n' > "$S16/doc/page.md"; printf '<svg/>\n' > "$S16/doc/logo.svg"; printf '[]\n' > "$S16/doc/urlmap.js"
cat > "$S16/build/doc/x.toml" <<EOF
[library]
namespace = "X"
logo_url = "logo.svg"

[extra]
urlmap_file = "urlmap.js"
content_files = [
  "page.md",
]
content_images = [
  "logo.svg",
]
EOF
cat > "$S16/build/build.ninja" <<EOF
rule gidoc
  command = $S16/gi-docgen generate --quiet --fatal-warnings --config doc/x.toml --output-dir=doc/X-1.0 --no-namespace-dir --content-dir=$S16/doc lib/X-1.0.gir
build doc/X-doc: gidoc doc/x.toml lib/X-1.0.gir | ../doc/page.md
EOF
drive "$NN" "$TASK" "$S16/build" doc/X-doc > "$D/gidoc.json" \
  || { echo "NO ANSWER: the gi-docgen drive did not emit a derivation"; tail -3 "$S16/build/drive.err"; exit 2; }
G_PAGE=$(carries "$D/gidoc.json" 'doc/page\.md')
G_IMG=$(carries "$D/gidoc.json" 'doc/logo\.svg')
G_MAP=$(carries "$D/gidoc.json" 'doc/urlmap\.js')
echo "gi-docgen carries page(floor)=$G_PAGE image=$G_IMG urlmap=$G_MAP"

# ---- ARM 17: the files a Doxyfile names, resolved against the cwd -----------
# MEASURED 2026-09-26 on rapidjson (consumer ledger, task `1yyv38wq`):
# `cd /build/source && doxygen /build/source/build/doc/Doxyfile` died
# `header file './doc/misc/header.html' does not exist`; the edge declared
# the Doxyfile and 28 generated files, nothing under `doc/misc`. doxygen
# resolves against its CWD, not the Doxyfile's dir (proven both ways there).
# Argv shape from the ledger and from libjxl's kept tree (`cd <dir> &&
# doxygen <abs Doxyfile>`). The Doxyfile is the declared floor; the header
# (a file) and the include dir (a directory) are the subject.
S17=$D/doxy; mkdir -p "$S17/build/doc" "$S17/doc/misc" "$S17/include/rj"
printf '#!/bin/sh\nmkdir -p html\n' > "$S17/doxygen"; chmod +x "$S17/doxygen"
printf '<html/>\n' > "$S17/doc/misc/header.html"; printf '# readme\n' > "$S17/readme.md"
printf '#define RJ 1\n' > "$S17/include/rj/rj.h"
cat > "$S17/build/doc/Doxyfile" <<EOF
INPUT                  = readme.md \\
                         include/
RECURSIVE              = YES
HTML_HEADER            = ./doc/misc/header.html
HTML_FOOTER            =
EOF
cat > "$S17/build/build.ninja" <<EOF
rule doxy
  command = cd $S17 && $S17/doxygen $S17/build/doc/Doxyfile
build doc/html: doxy doc/Doxyfile
EOF
drive "$NN" "$TASK" "$S17/build" doc/html > "$D/doxy.json" \
  || { echo "NO ANSWER: the doxygen drive did not emit a derivation"; tail -3 "$S17/build/drive.err"; exit 2; }
X_CFG=$(carries "$D/doxy.json" 'doc/Doxyfile')
X_HDR=$(carries "$D/doxy.json" 'doc/misc/header\.html')
X_INC=$(carries "$D/doxy.json" 'include/rj/rj\.h')
echo "doxygen carries Doxyfile(floor)=$X_CFG header=$X_HDR include-dir-file=$X_INC"

# ---- ARM 18: the HEADERS and SOURCES an AutogenInfo.json lists ------------
# MEASURED 2026-10-16 on qtbase 6.11.2 (KDE round, task `cckhnws9`): the
# `QTlsBackendCertOnlyPlugin` autogen edge died `The header file
# "SRC:/src/plugins/tls/shared/qasn1element_p.h" does not exist`: the plan's
# HEADERS list names a file in a SIBLING of the target's source dir, which no
# MOC_INCLUDES directory covers. Argv shape from that task's drv (`cd <build
# subdir> && cmake -E cmake_autogen <abs AutogenInfo.json> Release && cmake -E
# touch <abs timestamp>`); entry shape `[path, flags, ...]` from qtbase's kept
# Core plan. own.h rides the MOC_INCLUDES directory route (the floor); the
# sibling header is the subject.
S18=$D/autogen; A18=$S18/build/src/tls/certonly/CMakeFiles/P_autogen.dir
mkdir -p "$S18/src/tls/certonly" "$S18/src/tls/shared" "$A18" "$S18/build/src/tls/certonly/P_autogen"
printf '#pragma once\n' > "$S18/src/tls/certonly/own.h"
printf 'int c;\n' > "$S18/src/tls/certonly/cert.cpp"
printf '#pragma once\n' > "$S18/src/tls/shared/asn_p.h"
cat > "$A18/AutogenInfo.json" <<EOF
{
  "HEADERS": [["$S18/src/tls/shared/asn_p.h", "Mu", "X/moc_asn_p.cpp", null]],
  "SOURCES": [["$S18/src/tls/certonly/cert.cpp", "Mu", null]],
  "MOC_INCLUDES": ["$S18/src/tls/certonly"],
  "CMAKE_SOURCE_DIR": "$S18",
  "CMAKE_BINARY_DIR": "$S18/build"
}
EOF
cat > "$S18/build/build.ninja" <<EOF
rule ag
  command = cd $S18/build/src/tls/certonly && $CMAKE_BIN -E cmake_autogen $A18/AutogenInfo.json Release && $CMAKE_BIN -E touch $S18/build/src/tls/certonly/P_autogen/timestamp
build src/tls/certonly/P_autogen/timestamp: ag src/tls/certonly/CMakeFiles/P_autogen.dir/AutogenInfo.json
EOF
drive "$NN" "$TASK" "$S18/build" src/tls/certonly/P_autogen/timestamp > "$D/autogen.json" \
  || { echo "NO ANSWER: the autogen drive did not emit a derivation"; tail -3 "$S18/build/drive.err"; exit 2; }
A_OWN=$(carries "$D/autogen.json" 'certonly/own\.h')
A_SIB=$(carries "$D/autogen.json" 'tls/shared/asn_p\.h')
echo "autogen carries own.h(floor)=$A_OWN sibling-header=$A_SIB"

# ---- ARM 18b: an autogen edge's own earlier output, on a second pass ------
# MEASURED 2026-10-17 on qtbase 6.11.2 (task `kw0ja5vj`, the shim's install
# pass): the plan's MOC_INCLUDES names the target's own build dir, so the walk
# reached `<T>_autogen/include` that the plain pass had placed, and moc died
# EACCES x144 on its own outputs placed read-only. The subject is the placed
# moc file and ParseCache.txt beneath the edge's two autogen trees; the plan
# must survive (it lives beneath one of them) and a configure file in the
# walked dir is the floor that proves the walk ran.
S18B=$D/autogen2; B18=$S18B/build/src/core; A18B=$B18/CMakeFiles/Q_autogen.dir
mkdir -p "$S18B/src/core" "$A18B" "$B18/Q_autogen/include"
printf 'int q;\n' > "$S18B/src/core/q.cpp"
printf '#define CFG 1\n' > "$B18/cfg.h"
printf '/* moc */\n' > "$B18/Q_autogen/include/moc_q.cpp"
printf 'cache\n' > "$A18B/ParseCache.txt"
cat > "$A18B/AutogenInfo.json" <<EOF
{
  "SOURCES": [["$S18B/src/core/q.cpp", "Mu", null]],
  "MOC_INCLUDES": ["$B18"],
  "CMAKE_SOURCE_DIR": "$S18B",
  "CMAKE_BINARY_DIR": "$S18B/build"
}
EOF
cat > "$S18B/build/build.ninja" <<EOF
rule ag
  command = cd $B18 && $CMAKE_BIN -E cmake_autogen $A18B/AutogenInfo.json Release && $CMAKE_BIN -E touch $B18/Q_autogen/timestamp
build src/core/Q_autogen/timestamp src/core/Q_autogen/mocs_compilation.cpp: ag src/core/CMakeFiles/Q_autogen.dir/AutogenInfo.json
EOF
drive "$NN" "$TASK" "$S18B/build" src/core/Q_autogen/timestamp > "$D/autogen2.json" \
  || { echo "NO ANSWER: the second-pass autogen drive did not emit a derivation"; tail -3 "$S18B/build/drive.err"; exit 2; }
B_CFG=$(carries "$D/autogen2.json" 'src/core/cfg\.h')
B_PLAN=$(carries "$D/autogen2.json" 'Q_autogen\.dir/AutogenInfo\.json')
B_MOC=$(carries "$D/autogen2.json" 'Q_autogen/include/moc_q\.cpp')
B_CACHE=$(carries "$D/autogen2.json" 'Q_autogen\.dir/ParseCache\.txt')
echo "autogen pass 2 carries cfg.h(floor)=$B_CFG plan=$B_PLAN own-moc=$B_MOC own-cache=$B_CACHE"

# ---- ARM 18c: a syncqt edge's own earlier output, on a second pass -------
# MEASURED 2026-10-17 on qtbase 6.11.2 (task `3i6i6j4p`, the shim's install
# pass): the walk swept `include/QtWaylandGlobal/QtWaylandGlobalVersion`,
# which the plain pass's syncqt had written and nothing declares, back into
# the same edge as a read-only store link, and syncqt died "Unable to write
# header file alias". Configure writes `<Module>Depends` into the same tree
# BEFORE build.ninja; that file is the floor, the placed alias and the
# staging copy (newer than build.ninja) are the subject. The mtimes are set
# explicitly because the drop's boundary is the manifest's mtime.
# THE BLANKET IS THE ROUTE production took (7436 inputs), so this one drive
# turns it back on; every other arm keeps it off.
S18C=$D/syncqt2; B18C=$S18C/build; I18C=$B18C/include/QtX
mkdir -p "$I18C/.syncqt_staging" "$B18C/src/x"
printf '#include <QtCore/QtCore>\n' > "$I18C/QtXDepends"
printf '#include "qtxversion.h"\n' > "$I18C/QtXVersion"
printf '#include "qtxversion.h"\n' > "$I18C/.syncqt_staging/QtXVersion"
cat > "$B18C/build.ninja" <<EOF
rule sq
  command = cd $B18C && $CMAKE_BIN -E touch include/QtX/QtX src/x/X_syncqt_timestamp
build include/QtX/QtX src/x/X_syncqt_timestamp: sq
EOF
touch -d '-2 hours' "$I18C/QtXDepends"
touch -d '-1 hour' "$B18C/build.ninja"
DRIVE_IMPLICIT_LIMIT=100000 drive "$NN" "$TASK" "$B18C" src/x/X_syncqt_timestamp > "$D/syncqt2.json" \
  || { echo "NO ANSWER: the second-pass syncqt drive did not emit a derivation"; tail -3 "$B18C/drive.err"; exit 2; }
C18_DEP=$(carries "$D/syncqt2.json" 'include/QtX/QtXDepends')
C18_VER=$(carries "$D/syncqt2.json" 'include/QtX/QtXVersion')
C18_STG=$(carries "$D/syncqt2.json" 'syncqt_staging/QtXVersion')
echo "syncqt pass 2 carries Depends(floor)=$C18_DEP own-alias=$C18_VER own-staging=$C18_STG"
# ARM 18c: the configure-time Depends is the floor; the alias and staging
# copy an earlier pass placed are the subject. Scored HERE, beside its drive,
# so a later arm that exits early (ARM 21: NO ANSWER) cannot shadow it. A pin older than the fix
# carries both, which is the control that the fixture models the class.
if [ "${C18_DEP:-0}" -lt 1 ]; then
  echo "FAIL: the second-pass syncqt task lost its configure-time Depends (dep=$C18_DEP)"
  exit 1
fi
if [ "${C18_VER:-0}" -ge 1 ] || [ "${C18_STG:-0}" -ge 1 ]; then
  echo "FAIL: a syncqt edge carries its own earlier output as an input (alias=$C18_VER staging=$C18_STG) - syncqt cannot rewrite a store link"
  exit 1
fi

# ---- ARM 19: a `cmake -P` script's COMPOSED includes, read in order -------
# opencv 4.13.0's `opencv4.pc` edge runs
# `cd <build> && cmake -DCMAKE_HELPER_SCRIPT=<build>/OpenCVGenPkgConfig.info.cmake
# -P <src>/cmake/OpenCVGenPkgconfig.cmake`. The script does
# `include("${CMAKE_HELPER_SCRIPT}")`, and the helper does
# `set(OpenCV_SOURCE_DIR ...)`. The next include composes from that name, and
# the module it opens includes `vars/EnableModeVars.cmake` through
# `CMAKE_CURRENT_LIST_DIR`. The edge declares the helper alone; before
# `cmake_script_included_paths` the task carried neither the module nor the
# vars file, and cmake stopped with "include could not find requested file".
# `cmake_script_referenced_paths` reads only one level, so the first
# `include` is its floor, and the second, composed from the helper's `set`,
# is the subject. The vars file is the third level, `CMAKE_CURRENT_LIST_DIR`,
# which is the module reader's one-level limit. The script itself is the
# floor: the reader pushes it as an input before the reads below it, so it
# must arrive whatever the fix does.
# MUTATION CHECK: delete the `referenced.extend(cmake_script_included_paths(`
# call and this arm's subject counts read 0, so the arm catches it.
# THE HELPER SITS BESIDE $D, NOT UNDER IT, AND THAT PLACEMENT IS THE ARM.
# The driver textually rewrites every in-tree absolute path in the command
# to a build-dir-relative spelling before the readers see it
# (`rewrite_ancestor_paths_ups`), so a helper under $S19/build would reach
# the reader as the relative `helper.cmake` - and the reader only OPENS an
# absolute expansion, so the helper's `set()` would never fold, the module
# include would never expand, and the arm would read 0 at HEAD for a reason
# that is not about the reader. Production never rewrites: its build dir is
# under /build, where the driver keeps the original argv. A sibling of $D
# shares only /tmp with the build dir, which is below the rewrite's
# 3-component cutoff, so the -D value stays absolute; `same_project_tree`
# still passes on the shared first component. Do not "simplify" the helper
# back under $S19: the arm goes red and the red says nothing.
S19=$D/cmkinc; M19=$S19/src/cmake/vars; H19=$D-helper; mkdir -p "$S19/build" "$S19/src/cmake" "$M19" "$H19"
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D" "$H19" "$D-cmkcopy-cef"' EXIT
printf 'set(BUILD_SHARED_LIBS "ON")\nset(OpenCV_SOURCE_DIR "%s")\n' "$S19/src" > "$H19/helper.cmake"
printf 'include(CMakeParseArguments)\ninclude("${CMAKE_CURRENT_LIST_DIR}/vars/EnableModeVars.cmake")\n' > "$S19/src/cmake/OpenCVUtils.cmake"
printf 'enable_mode_vars()\n' > "$M19/EnableModeVars.cmake"
printf 'cmake_policy(SET CMP0012 NEW)\ninclude("${CMAKE_HELPER_SCRIPT}")\ninclude("${OpenCV_SOURCE_DIR}/cmake/OpenCVUtils.cmake")\n' > "$S19/src/cmake/script.cmake"
# The helper arrives as the edge's declared input and through the -D
# definition; the module and the vars file arrive only through the reader.
printf 'rule pc\n  command = %s -DCMAKE_HELPER_SCRIPT=%s/helper.cmake -P %s/src/cmake/script.cmake\nbuild opencv4.pc: pc %s/helper.cmake\n' \
  "$CMAKE_BIN" "$H19" "$S19" "$H19" > "$S19/build/build.ninja"
drive "$NN" "$TASK" "$S19/build" opencv4.pc > "$D/cmkinc.json" \
  || { echo "NO ANSWER: the cmake -P composed-include drive did not emit a derivation"; tail -3 "$S19/build/drive.err"; exit 2; }
C_SCRIPT=$(carries "$D/cmkinc.json" 'cmake/script\.cmake')
C_HELPER=$(carries "$D/cmkinc.json" 'helper\.cmake')
C_MODULE=$(carries "$D/cmkinc.json" 'cmake/OpenCVUtils\.cmake')
C_VARS=$(carries "$D/cmkinc.json" 'vars/EnableModeVars\.cmake')

# ---- ARM 20: the tools a shell script RUNS, resolved on the caller's PATH -
# packagekit 1.4.0's ref.xml edge runs `/bin/sh <src>/spec-to-docbook.sh ...`,
# and the script's line 7 is `xsltproc ${xsl} ${xml} | tail -n +2 >> ${out}`.
# Neither `xsltproc` nor `tail` was on the task's PATH; `tail` is coreutils
# (already carried), `xsltproc` is not, and the pipeline's status was
# `tail`'s, so the edge "succeeded" with a header-only ref.xml that gtk-doc
# rejected at install. `script_bare_programs` reads the script and resolves
# each name it runs on the caller's PATH; a store hit becomes a PATH entry
# and an input. WHAT THIS ASSERTS: the fix inserts the tool's store path as
# an Opaque input (which lands in `inputs.srcs`) AND pushes its `bin` dir
# onto the task's PATH - a fixture that scored the input list alone would
# read the tool here AND at the pin, because arm 4's XDG_DATA_DIRS shape
# carries the closure either way. Arm 7 scores the same shape for the
# unnamed compiler.
#
# `xsltproc` comes from nixpkgs' `libxslt`; its `bin` output is not named in
# the flake, so the store is asked for it the way arm 4 asks for ninja and
# arm 6 for glib-mkenums. THE TOOL MUST BE RESOLVABLE ON THE GATE'S PATH:
# `script_bare_programs` reads the names the script runs and asks the
# caller's PATH for them; the libxslt `bin` dir is appended to the drive's
# PATH so `xsltproc` lands in the store rather than `/usr/bin`, and the
# reader is the only route that carries it into the emitted derivation.
# `tail` is coreutils and arrives always; the xsltproc entry is the one a
# broken reader drops. The subject is the STORE PATH NAME of the libxslt
# bin output, resolved at run time: a version move in nixpkgs moves the
# whole name, and the subject then reads 0 on both sides, which is the
# fixture's own correct answer.
# MUTATION CHECK: delete the `for name in script_bare_programs(cmdline)` loop
# in `build_task_derivation` and `S_XSLT_SRC` and `S_XSLT_PATH` read 0, so
# the arm catches it.
XS=$(store_find '*-libxslt-*-bin')
XS_NAME=${XS##*/}
# A missing tool is NO ANSWER for the whole gate, the way arm 4 answers when
# its ninja is absent and arm 6 when its glib-mkenums is: the gate promises
# this arm, and a PASS without it would be coverage nobody has.
[ -n "$XS" ] && [ -x "$XS/bin/xsltproc" ] || { echo "NO ANSWER: libxslt's bin output is not in the store, so arm 20 cannot run"; exit 2; }
S20=$D/s2d; mkdir -p "$S20/build"
printf '#!/bin/sh\nspec_to_docbook_xsl="$1"\ninput_file="$2"\noutput_file="$3"\n\n' > "$S20/build/spec-to-docbook.sh"
printf 'xsltproc ${spec_to_docbook_xsl} ${input_file} | tail -n +2 >> ${output_file}\n' >> "$S20/build/spec-to-docbook.sh"
printf 'rule s2d\n  command = /bin/sh %s/build/spec-to-docbook.sh a.xsl b.xml out.xml\nbuild out.xml: s2d %s/build/spec-to-docbook.sh\n' "$S20" "$S20" > "$S20/build/build.ninja"
( cd "$S20/build"
  PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin:$XS/bin" \
  NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
  NIX_CONFIG="experimental-features = $FEAT" \
  NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
  "$NN/bin/nix-ninja" -t drv out.xml > "$D/s2d.json" 2>"$S20/build/drive.err"
) || { echo "NO ANSWER: the shell-script drive did not emit a derivation"; tail -3 "$S20/build/drive.err"; exit 2; }
S_SCRIPT=$(carries "$D/s2d.json" 'spec-to-docbook\.sh')
# WHAT THIS ASSERTS: the fix inserts the tool's store path as an Opaque
# input (which lands in `inputs.srcs`) AND pushes its `bin` dir onto the
# task's PATH - arm 7 scores the same shape for the unnamed compiler.
# `tail` is coreutils and arrives always; the xsltproc entry is the one a
# broken reader drops. The subject is the STORE PATH NAME of the libxslt
# bin output, resolved at run time: a version move in nixpkgs moves the
# whole name, and the subject then reads 0 on both sides, which is the
# fixture's own correct answer.
S_XSLT_SRC=$(carries "$D/s2d.json" "$XS_NAME")
S_XSLT_PATH=$(python3 -c "
import json,sys
try: d=json.load(open('$D/s2d.json'))
except Exception: print(0); sys.exit()
print(int(any('$XS_NAME/bin' in p for v in d.values() for p in v['env'].get('PATH','').split(':'))))")
# THE NEGATIVE CONTROL: the same script passed to `cat`, which does not run
# it. `cat` is coreutils, already on PATH; the script still arrives as an
# input to `cat`. `script_bare_programs` must read nothing from the
# control's command, so the control must NOT gain the xsltproc PATH entry.
# A drive that adds the entry anyway is carrying the tool from the wrong
# place, and the arm catches it.
S20CTL=$D/s2dctl; mkdir -p "$S20CTL/build"
printf '#!/bin/sh\nspec_to_docbook_xsl="$1"\ninput_file="$2"\noutput_file="$3"\n\n' > "$S20CTL/build/spec-to-docbook.sh"
printf 'xsltproc ${spec_to_docbook_xsl} ${input_file} | tail -n +2 >> ${output_file}\n' >> "$S20CTL/build/spec-to-docbook.sh"
printf 'rule s2dcat\n  command = cat %s/build/spec-to-docbook.sh\nbuild out.xml: s2dcat %s/build/spec-to-docbook.sh\n' "$S20CTL" "$S20CTL" > "$S20CTL/build/build.ninja"
# THE CONTROL MUST BE ON THE SAME DRIVE PATH AS THE SUBJECT: a tool that
# is not on the caller's PATH contributes nothing, and a drive that adds
# the libxslt bin only for the subject would read the control 0 for a
# reason that is not about the reader.
( cd "$S20CTL/build"
  PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin:$XS/bin" \
  NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
  NIX_CONFIG="experimental-features = $FEAT" \
  NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
  "$NN/bin/nix-ninja" -t drv out.xml > "$D/s2dctl.json" 2>"$S20CTL/build/drive.err"
) || { echo "NO ANSWER: the cat control drive did not emit a derivation"; tail -3 "$S20CTL/build/drive.err"; exit 2; }
S_CTL_SCRIPT=$(carries "$D/s2dctl.json" 'spec-to-docbook\.sh')
S_CTL_XSLT_SRC=$(carries "$D/s2dctl.json" "$XS_NAME")
S_CTL_XSLT_PATH=$(python3 -c "
import json,sys
try: d=json.load(open('$D/s2dctl.json'))
except Exception: print(0); sys.exit()
print(int(any('$XS_NAME/bin' in p for v in d.values() for p in v['env'].get('PATH','').split(':'))))")

# ---- ARM 21: vapigen's `<library>.vapi`, an undeclared output -------------
# libayatana-appindicator 0.5.92's rule declares `AyatanaAppIndicator3-0.1.vala`
# as its output, but `vapigen --library=ayatana-appindicator3-0.1` writes
# `ayatana-appindicator3-0.1.vapi` beside it. The install step reads the `.vapi`
# undeclared, and real ninja checks neither name. Two halves of the fix:
# `vapigen_vapi` makes the `.vapi` an undeclared OUTPUT that rides the edge
# (`NIX_NINJA_OUTPUTS`), and `vapigen_unwritten_outputs` stamps the declared
# name vapigen never writes, so the command ends in `&& touch '<build>/
# AyatanaAppIndicator3-0.1.vala'` (`drv.args[0]`).
#
# THE TOOL IS A STUB. `vapigen_vapi` matches on the token whose file name is
# `vapigen`; the reader fires on any such token, store or not, and the stub
# keeps the fixture store-free. The stub is named `vapigen` beside the build
# root, the shape the real edge carries (`<vala>/bin/vapigen`). The command
# names it by absolute host path, which stays in `args[0]` byte for byte,
# so the `&& touch` suffix is greppable without depending on a store version.
# `drive` sets `NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0`, so the `.vapi` reaches
# the derivation only through the reader.
#
# THE FIXTURE'S VALUES ARE DERIVED FROM THE SUBJECT: the `.vapi` spelling
# `bindings/vala/ayatana-appindicator3-0.1.vapi` is what `vapigen_vapi`
# answers on this command; the arm asserts that spelling and nothing else.
# MUTATION CHECK: delete the `vapigen_vapi` call inside `undeclared_outputs`
# and `AV_VAPI_OUT` reads 0; delete the `let unwritten =` block in
# `build_task_derivation` and `AV_TOUCH` reads 0. The third half of the fix,
# the `.chain(vapigen_vapi(...))` in `new_task` that hands the `.vapi` to
# CONSUMERS through co-outputs, is not observable through a single-target
# `-t drv` drive, so this arm does not claim it.
S21=$D/vapigen2
mkdir -p "$S21/build/bindings/vala" "$S21/build/tools" "$S21/build/src"
printf '<repository/>\n' > "$S21/build/src/AyatanaAppIndicator3-0.1.gir"
printf '#!/bin/sh\n: > "$2"\n' > "$S21/build/tools/vapigen"; chmod +x "$S21/build/tools/vapigen"
# The real argv shape: `cd` into the dir the `.vapi` lands in, the tool
# named by an absolute host path (the stub stands in for the store tool
# the real edge names). The stub lives UNDER the build root: a stub beside
# it breaks program resolution, because the rewrite respells the program
# against the command's cd target while `which` resolves from the drive's
# cwd, and the two agree only under the build root. That mismatch is
# fixture-only (production runs the original command line), so the arm
# carries the layout that resolves.
printf 'rule vapi\n  command = cd %s/build/bindings/vala && %s/build/tools/vapigen --library=ayatana-appindicator3-0.1 --pkg gtk+-3.0 %s/build/src/AyatanaAppIndicator3-0.1.gir\nbuild bindings/vala/AyatanaAppIndicator3-0.1.vala: vapi %s/build/src/AyatanaAppIndicator3-0.1.gir\n' \
  "$S21" "$S21" "$S21" "$S21" > "$S21/build/build.ninja"
drive "$NN" "$TASK" "$S21/build" bindings/vala/AyatanaAppIndicator3-0.1.vala > "$D/vapigen2.json" \
  || { echo "NO ANSWER: the vapigen vapi drive did not emit a derivation"; tail -3 "$S21/build/drive.err"; exit 2; }
# ASSERT: NIX_NINJA_OUTPUTS carries the .vapi, and the emitted command line
# ends with the `&& touch` stamp for the declared name. The JSON the driver
# prints carries `args` as a string list, so the stamp is greppable over the
# JSON itself.
AV_VAPI_OUT=$(carries "$D/vapigen2.json" 'ayatana-appindicator3-0.1\.vapi')
AV_TOUCH=$(carries "$D/vapigen2.json" "touch '[^']*AyatanaAppIndicator3-0[.]1[.]vala'")
AV_VALA_OUT=$(carries "$D/vapigen2.json" 'AyatanaAppIndicator3-0\.1\.vala')
AV_GIR=$(carries "$D/vapigen2.json" 'AyatanaAppIndicator3-0.1\.gir')

# ---- ARM 22: the files a .qrc lists, and the climbs it traverses ------------
# MEASURED 2026-10-18 on qttools 6.11.2 (task
# `kjfa9zlgnrbn8nx0bbwk4nkmwq81ydzj-ninja-build.drv`): `rcc` died
# `Cannot find file '.../src/designer/src/designer/../../../shared/...'`
# although the normalized target was materialized - the kernel resolves each
# `..` against a directory that must EXIST, and `src/designer/src/designer`
# is declared by nothing. breeze-icons 6.30.0's pass 1 died the same way on
# entries no edge declares at all, and qtquick3d 6.11.2's on a generated
# `.qsbc` no scan can see. The tool is a stub; commands never run under
# -t drv. The fixture's geometry is production's: the qrc lives under the
# build dir, one entry climbs out through nonexistent intermediates to a
# source-tree file and one is plain relative to the command's cwd. A
# generated member is NOT in this fixture; it has its own twin below,
# because its wiring (defer, re-queue, bind) is a different claim from
# these spellings.
S22=$D/qrc; mkdir -p "$S22/build/icons" "$S22/build/.qt/rcc" "$S22/src/shared/img" "$S22/build/tools"
printf '#!/bin/sh\n: > "$2"\n' > "$S22/build/tools/rcc"; chmod +x "$S22/build/tools/rcc"
printf '<svg/>\n' > "$S22/src/shared/img/back.png"
printf 'data\n' > "$S22/build/icons/res.dat"
cat > "$S22/build/.qt/rcc/app.qrc" <<EOF
<RCC>
  <qresource prefix="/">
    <file alias="images/back.png">$S22/src/c/icons/sub/../../../shared/img/back.png</file>
    <file>res.dat</file>
  </qresource>
</RCC>
EOF
cat > "$S22/build/build.ninja" <<EOF
rule rcc
  command = cd $S22/build/icons && $S22/build/tools/rcc --output \$out --name app $S22/build/.qt/rcc/app.qrc
build icons/qrc_app.cpp: rcc .qt/rcc/app.qrc
EOF
drive "$NN" "$TASK" "$S22/build" icons/qrc_app.cpp > "$D/qrc.json" \
  || { echo "NO ANSWER: the qrc drive did not emit a derivation"; tail -3 "$S22/build/drive.err"; exit 2; }
Q_CLIMB=$(carries "$D/qrc.json" 'shared/img/back\.png')
Q_REL=$(carries "$D/qrc.json" 'icons/res\.dat')
Q_DIRS=$(carries "$D/qrc.json" 'src/c/icons/sub')
echo "qrc climb=$Q_CLIMB relative=$Q_REL climb-dirs=$Q_DIRS"
# THE NO-CLIMB CONTROL: a qrc whose entries stay under the build dir must
# add NO make-dirs, or every passing rcc task re-keys. Same drive, same
# command shape, plain entries only.
cat > "$S22/build/.qt/rcc/plain.qrc" <<EOF
<RCC>
  <qresource prefix="/">
    <file>res.dat</file>
  </qresource>
</RCC>
EOF
cat > "$S22/build/build2.ninja" <<EOF
rule rcc
  command = cd $S22/build/icons && $S22/build/tools/rcc --output \$out --name plain $S22/build/.qt/rcc/plain.qrc
build icons/qrc_plain.cpp: rcc .qt/rcc/plain.qrc
EOF
( cd "$S22/build"
  PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin" \
  NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
  NIX_CONFIG="experimental-features = $FEAT" \
  NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
  "$NN/bin/nix-ninja" -f build2.ninja -t drv icons/qrc_plain.cpp > "$D/qrc.ctl.json" 2>"$S22/build/drive.err"
) || { echo "NO ANSWER: the no-climb qrc drive did not emit a derivation"; tail -3 "$S22/build/drive.err"; exit 2; }
Q_CTL_DIRS=$(carries "$D/qrc.ctl.json" 'NIX_NINJA_MAKE_DIRS')
Q_CTL_RES=$(carries "$D/qrc.ctl.json" 'icons/res\.dat')
echo "qrc control make-dirs-key=$Q_CTL_DIRS relative=$Q_CTL_RES"

# ---- ARM 22b: an UNDECLARED generated member of an ON-DISK qrc DEFERS, ----
# re-queues, and BINDS
# MEASURED 2026-10-18 on qtquick3d 6.11.2: `res_particle_shaders.qrc`
# itself is generated, and the `.qsbc` it lists is too. SCOPE NOTE: this arm
# covers the ON-DISK half only (the fixture's `app.qrc` is hand-written, so
# the producing edge resolves late but the qrc reads at both resolutions).
# The entry is deliberately NON-CLIMBING (`gen.dat`, bound through the
# command-dir join): aaefd48's candidate join does no normalization, so a
# climbing entry could never bind there whatever the deferral did, and the
# control would not isolate the deferral. The climbing spelling is covered
# by the `a_climbing_entry` unit test. A generated qrc binds through the
# same deferral (arm 22d asserts the qrc-bind half), but its members stay
# unread until it exists on disk: single-pass they are NOT carried. That
# half of the shape stays open, not modeled.
# WHAT THIS ASSERTS: the undeclared-member drive EMITS a derivation carrying
# the member (`icons/gen.dat`), reached THROUGH DEFERRAL, while the same
# drive at aaefd48 emits WITHOUT it (the old silent drop: the reader is
# present there and the deferral is not). The defer line IS scored: for
# this drive deferral is the only route (the member's producer is
# undeclared, so it cannot resolve before the consumer and a direct bind
# is impossible). The declared twin carries at HEAD and at aaefd48 alike
# (the generic route is live where the subject runs). No pin drive: the
# pin's driver predates the tool handling this fixture needs and cannot
# run it at all (measured: `Failed to find ../tools/rcc`), so scoring a
# pin emission would read a tool gap as the class. The qrc-file-itself
# half binds the qrc the same way and carries no members this pass (arm
# 22d); both halves are asserted through the spelling the producer's edge
# outputs, never a store path or graph id.
S22B=$D/qrcdefer; mkdir -p "$S22B/build/icons" "$S22B/build/.qt/rcc" "$S22B/build/tools"
printf '#!/bin/sh\n: > "$2"\n' > "$S22B/build/tools/rcc"; chmod +x "$S22B/build/tools/rcc"
cat > "$S22B/build/.qt/rcc/app.qrc" <<EOF
<RCC>
  <qresource prefix="/">
    <file>gen.dat</file>
  </qresource>
</RCC>
EOF
cat > "$S22B/build/build.ninja" <<EOF
rule touch
  command = touch \$out
rule rcc
  command = cd $S22B/build/icons && $S22B/build/tools/rcc --output \$out --name app $S22B/build/.qt/rcc/app.qrc
build icons/gen.dat: touch
build icons/qrc_app.cpp: rcc .qt/rcc/app.qrc
EOF
( cd "$S22B/build"
  PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin" \
  NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
  NIX_CONFIG="experimental-features = $FEAT" \
  NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
  "$NN/bin/nix-ninja" -t drv icons/qrc_app.cpp > "$D/qrcdefer.json" 2>"$S22B/build/drive.err"
) || { echo "NO ANSWER: the generated-member undeclared drive did not emit a derivation"; tail -3 "$S22B/build/drive.err"; exit 2; }
QB_DEFER=$(command grep -c 'deferred .* until the producer of' "$S22B/build/drive.err" 2>/dev/null || true)
QB_MISS=$(carries "$D/qrcdefer.json" 'icons/gen\.dat')
# Keep the HEAD drive's stderr: the pin and aaefd48 drives below reuse the
# same directory and its fixed drive.err name, overwriting it.
cp "$S22B/build/drive.err" "$D/qrcdefer.head.err"
# The twin where the producer is DECLARED: same geometry, the qrc names
# the member at a spelling the producing edge outputs, and the edge
# declares it as an input. This is the CONTROL: a declared input rides
# the generic route at every revision, so it must carry at HEAD, at the
# pin, and at aaefd48 alike. Its carriage is what makes silence on the
# undeclared half the defect rather than a broken fixture.
cat > "$S22B/build/build2.ninja" <<EOF
rule touch
  command = touch \$out
rule rcc
  command = cd $S22B/build/icons && $S22B/build/tools/rcc --output \$out --name app $S22B/build/.qt/rcc/plain.qrc
build icons/gen.dat: touch
build icons/qrc_plain.cpp: rcc .qt/rcc/plain.qrc icons/gen.dat
EOF
cat > "$S22B/build/.qt/rcc/plain.qrc" <<EOF
<RCC>
  <qresource prefix="/">
    <file>gen.dat</file>
  </qresource>
</RCC>
EOF
( cd "$S22B/build"
  PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin" \
  NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
  NIX_CONFIG="experimental-features = $FEAT" \
  NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
  "$NN/bin/nix-ninja" -f build2.ninja -t drv icons/qrc_plain.cpp > "$D/qrcdefer.bind.json" 2>"$S22B/build/drive2.err"
) || { echo "NO ANSWER: the generated-member bind drive did not emit a derivation"; tail -3 "$S22B/build/drive2.err"; exit 2; }
QB_BIND=$(carries "$D/qrcdefer.bind.json" 'icons/gen\.dat')
echo "qrc defer-line=$QB_DEFER undeclared-carry=$QB_MISS declared-carry=$QB_BIND"
# ---- ARM 22c: an edge's OWN qrc output is never its input ----------------
# `rcc -project -o gen.qrc` names the file it writes. Absent at scan time,
# so without the self-output guard the edge defers on itself and the
# scheduler aborts on the cycle. WHAT THIS ASSERTS: the drive emits and
# defers nothing. If the guard is ever removed the drive aborts and the
# failure branch below scores FAIL on `deferral refused`, which is the
# pin tripping, not a silent pass.
S22C=$D/qrcself; mkdir -p "$S22C/build"
cat > "$S22C/build/build.ninja" <<EOF
rule rcc
  command = cd $S22C/build && $S22B/build/tools/rcc -project -o gen.qrc
build gen.qrc: rcc
EOF
( cd "$S22C/build"
  PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin" \
  NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
  NIX_CONFIG="experimental-features = $FEAT" \
  NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
  "$NN/bin/nix-ninja" -t drv gen.qrc > "$D/qrcself.json" 2>"$S22C/build/drive.err"
) || { if command grep -q 'deferral refused' "$S22C/build/drive.err" 2>/dev/null; then echo "FAIL: the edge deferred on its own qrc output - the self-output guard is gone"; exit 1; fi; echo "NO ANSWER: the self-output qrc drive did not emit a derivation"; tail -3 "$S22C/build/drive.err"; exit 2; }
QS_OUT=$(carries "$D/qrcself.json" 'NIX_NINJA_OUTPUTS')
QS_DEFER=$(command grep -c 'deferred .* until the producer of' "$S22C/build/drive.err" 2>/dev/null || true)
echo "qrc self-output outputs-key=$QS_OUT defer-line=$QS_DEFER"
# ---- ARM 22d: a GENERATED qrc binds through deferral (members stay open) --
# The qrc-file push half, unexercised until now: edge1 writes `app.qrc`
# (generated, absent at scan), edge2 consumes it over an absolute path.
# WHAT THIS ASSERTS: edge2's drive EMITS through deferral carrying the qrc
# itself (`app.qrc` in NIX_NINJA_INPUTS). The source member the qrc lists
# is NOT asserted: the qrc is unreadable at (re-)resolution, so it never
# becomes a candidate single-pass (open shape). If the push is ever
# removed, edge2 emits without the qrc and QD_QRC scores it.
S22D=$D/qrcgen; mkdir -p "$S22D/build/icons" "$S22D/build/data"
printf 'data\n' > "$S22D/build/data/lib.dat"
cat > "$S22D/build/build.ninja" <<EOF
rule write
  command = printf '<RCC><qresource prefix="/"><file>../data/lib.dat</file></qresource></RCC>' > \$out
rule rcc
  command = cd $S22D/build/icons && $S22B/build/tools/rcc --output \$out --name app $S22D/build/.qt/rcc/app.qrc
build .qt/rcc/app.qrc: write
build icons/qrc_app.cpp: rcc
EOF
( cd "$S22D/build"
  PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin" \
  NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
  NIX_CONFIG="experimental-features = $FEAT" \
  NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
  "$NN/bin/nix-ninja" -t drv icons/qrc_app.cpp > "$D/qrcgen.json" 2>"$S22D/build/drive.err"
) || { echo "NO ANSWER: the generated-qrc drive did not emit a derivation"; tail -3 "$S22D/build/drive.err"; exit 2; }
QD_DEFER=$(command grep -c 'deferred .* until the producer of' "$S22D/build/drive.err" 2>/dev/null || true)
QD_QRC=$(carries_inputs "$D/qrcgen.json" 'app.qrc')
echo "qrc generated defer-line=$QD_DEFER qrc-carry=$QD_QRC"

# ---- ARM 27: a grd candidate produced late defers, not drops ---------------
# MEASURED 2026-10-20 on qtwebengine 6.11.2 (task
# `gs146m566pgnz6yhskfjp2qdwzh9q4fk-ninja-build.drv`): the grit edge's
# generated grd names `Images/3d-center.svg.compressed`, which a sibling
# compress edge produces and no edge declares. The candidate names the graph
# node, but the consumer resolves before the producer owns a derivation, so
# the old code dropped it in silence and grit died FileNotFoundError on the
# missing file. Same shape as the arm-22b qrc member: on-disk manifest (so
# the candidate reads at both resolutions), produced member absent at scan
# (so the existence half cannot bind it), undeclared producer (so a direct
# bind is impossible and deferral is the only route).
# WHAT THIS ASSERTS: the drive EMITS carrying `member.dat` THROUGH DEFERRAL
# (the defer line is scored), while the same drive at the pin and at aaefd48
# emits WITHOUT it (the candidate loop is present in both and the deferral
# in neither, so silence there isolates the deferral as the delta). No store
# tools: both fixtures are stub-only, so both controls can run them.
S27=$D/grddefer; mkdir -p "$S27/build"
cat > "$S27/build/app.grd" <<EOF
<grit><include file="member.dat" name="APP_MEMBER" type="BINDATA"/></grit>
EOF
cat > "$S27/build/build.ninja" <<EOF
rule touch
  command = touch \$out
rule consume
  command = cat \$in > \$out
build member.dat: touch
build out.txt: consume app.grd
EOF
drive "$NN" "$TASK" "$S27/build" out.txt > "$D/grddefer.json" 2>"$S27/build/drive.err" || {
  echo "NO ANSWER: the grd-defer drive did not emit a derivation"; tail -3 "$S27/build/drive.err"; exit 2; }
GD_DEFER=$(command grep -c 'deferred .* until the producer of' "$S27/build/drive.err" 2>/dev/null || true)
GD_MISS=$(carries_inputs "$D/grddefer.json" 'member.dat')
GD_DECL=$(carries_inputs "$D/grddefer.json" 'app.grd')
echo "grd defer-line=$GD_DEFER undeclared-carry=$GD_MISS declared=$GD_DECL"

# ---- ARM 23: the DIRECTORIES of a `cmake -E copy`, on a link edge ---------
# MEASURED 2026-10-18 on obs-studio 32.2.2 (task
# `97ahxhrwdn9izawi4gfyj6lnwdkndijbzw-ninja-build.drv`): the obs-browser link
# chains `cmake -E copy_if_different` and `cmake -E copy_directory` of its
# CEF siblings, and no edge declares them. The `copy_if_different` tail is
# verbatim from the round log; the `copy_directory` names the leaf's own
# missing `Resources/locales`. The declared input file is the floor.
# THE CEF TREE SITS IN A DASH-SIBLING ROOT, NOT UNDER $D, and that
# placement is the arm: the rewrite relativizes absolute paths through
# every ancestor with three or more components, so a tree under $D is
# respelled through their common parent and the generic relative branch
# carries it even without the reader - the pin control could never fail.
# Production's `/build/cef` beside `/build/source/build` shares only
# `/build` with the build dir, which is below the cutoff, so the absolutes
# survive there; a `$D-cef` dash-sibling shares only `/tmp`, which is
# below the cutoff the same way (the arm-19 helper rides the identical
# boundary). A `$D/cmkcopy-cef` slash child would NOT work: `$D/` is a
# prefix of it and the rewrite fires through the common parent.
S23=$D/cmkcopy; S23C=$D-cmkcopy-cef
mkdir -p "$S23/build/out" "$S23C/cef/Release" "$S23C/cef/Resources/locales"
printf 'lib\n' > "$S23C/cef/Release/libcef.so"
printf 'dat\n' > "$S23C/cef/Release/data.dat"
printf 'loc\n' > "$S23C/cef/Resources/locales/en.pak"
printf 'in\n' > "$S23/build/in.txt"
cat > "$S23/build/build.ninja" <<EOF
rule link
  command = cd $S23/build && $CMAKE_BIN -E copy_if_different $S23C/cef/Release/libcef.so $S23C/cef/Release/data.dat out/ && $CMAKE_BIN -E copy_directory $S23C/cef/Resources/locales out/res && $CMAKE_BIN -E touch out/stamp
build out/stamp: link in.txt
EOF
drive "$NN" "$TASK" "$S23/build" out/stamp > "$D/cmkcopy.json" \
  || { echo "NO ANSWER: the cmake-copy drive did not emit a derivation"; tail -3 "$S23/build/drive.err"; exit 2; }
K_LIB=$(carries_inputs "$D/cmkcopy.json" 'cef/Release/libcef.so')
K_DAT=$(carries_inputs "$D/cmkcopy.json" 'cef/Release/data.dat')
K_LOC=$(carries_inputs "$D/cmkcopy.json" 'Resources/locales/en.pak')
K_IN=$(carries "$D/cmkcopy.json" 'in\.txt')
echo "cmake-copy lib=$K_LIB data=$K_DAT locales=$K_LOC declared=$K_IN"

# ---- ARM 24: an ExternalProject configure's SOURCE_DIR ----------------------
# MEASURED 2026-10-18 on qtwebengine 6.11.2 (task
# `6q476jql51mcyw4r9w7mybmcbh6q130v-ninja-build.drv`): `cmake -S <src>/src/gn`
# died on the absent dir, whose only record is the stamp sibling
# `gn-source_dirinfo.txt` (`method=source_dir`: no download, no extract
# step). Command, stamp layout and info content mirror the failing edge.
# The URL-method twin is the second arm. CMake names the info file after
# the method (`${name}-${method}info.txt`, ExternalProject's
# shared_internal_commands.cmake in the pinned cmake 4.4.3), so a URL
# download writes `<name>-urlinfo.txt` and never a `source_dirinfo.txt`,
# and with SOURCE_DIR outside the build dir no extract step declares the
# tree (`external_project_extract_dirs` refuses an out-of-tree
# destination). The generic directory-argument reader is its only carrier,
# reached since 6e2ccd3 rebased the `-S` climb onto the leading `cd`. An
# earlier control wrote `method=download` into `source_dirinfo.txt`, a
# state CMake never creates, and demanded 0; it went red at 6e2ccd3 for
# exactly the rebase that made the real shape work.
S24=$D/epsrc; mkdir -p "$S24/build/dep-build" "$S24/build/dep-prefix/src/dep-stamp" "$S24/src/dep"
printf 'cmake_minimum_required(VERSION 3.20)\n' > "$S24/src/dep/CMakeLists.txt"
printf 'method=source_dir\nsource_dir=%s/src/dep\n' "$S24" > "$S24/build/dep-prefix/src/dep-stamp/dep-source_dirinfo.txt"
cat > "$S24/build/build.ninja" <<EOF
rule cfg
  command = cd $S24/build/dep-build && $CMAKE_BIN -GNinja -S $S24/src/dep -B $S24/build/dep-build && $CMAKE_BIN -E touch $S24/build/dep-prefix/src/dep-stamp/dep-configure
build dep-prefix/src/dep-stamp/dep-configure: cfg
EOF
drive "$NN" "$TASK" "$S24/build" dep-prefix/src/dep-stamp/dep-configure > "$D/epsrc.json" \
  || { echo "NO ANSWER: the source_dir drive did not emit a derivation"; tail -3 "$S24/build/drive.err"; exit 2; }
E_SRC=$(carries "$D/epsrc.json" 'src/dep/CMakeLists')
echo "ep-sourcedir tree=$E_SRC"
S24B=$D/epsrcdl; mkdir -p "$S24B/build/dep-build" "$S24B/build/dep-prefix/src/dep-stamp" "$S24B/src/dep"
printf 'cmake_minimum_required(VERSION 3.20)\n' > "$S24B/src/dep/CMakeLists.txt"
printf 'method=url\ncommand=\nsource_dir=%s/src/dep\nwork_dir=%s/build/dep-prefix/src\n' "$S24B" "$S24B" > "$S24B/build/dep-prefix/src/dep-stamp/dep-urlinfo.txt"
cat > "$S24B/build/build.ninja" <<EOF
rule cfg
  command = cd $S24B/build/dep-build && $CMAKE_BIN -GNinja -S $S24B/src/dep -B $S24B/build/dep-build && $CMAKE_BIN -E touch $S24B/build/dep-prefix/src/dep-stamp/dep-configure
build dep-prefix/src/dep-stamp/dep-configure: cfg
EOF
drive "$NN" "$TASK" "$S24B/build" dep-prefix/src/dep-stamp/dep-configure > "$D/epsrc.dl.json" \
  || { echo "NO ANSWER: the url-method drive did not emit a derivation"; tail -3 "$S24B/build/drive.err"; exit 2; }
E_DL=$(carries "$D/epsrc.dl.json" 'src/dep/CMakeLists')
echo "ep-sourcedir url-method tree=$E_DL (must be at least 1)"

# ---- ARM 25: the html tree a doxygen stamp edge writes ----------------------
# MEASURED 2026-10-18 on kdsoap-ws-discovery-client 0.4.0: `doxygen_add_docs
# (docs ... ALL USE_STAMP_FILE)` runs `cmake -E make_directory <out> &&
# doxygen <Doxyfile> && cmake -E touch docs.stamp`; the task ran rc=0, the
# stamp was stored, the html tree stayed in the sandbox as an undeclared
# write, and install died `file INSTALL cannot find <build>/docs/html`.
# Same undeclared-side-product family as the copy reader's openblas header.
# The tool is a stub; commands never run under -t drv. The stamp is the
# floor, the html tree the subject, the GENERATE_HTML=NO twin the control
# that the reader answers the config rather than the command.
S25=$D/doxyout; mkdir -p "$S25/build/docs" "$S25/build/tools" "$S25/src/docs"
printf '#!/bin/sh\nmkdir -p "$3"\n' > "$S25/build/tools/doxygen"; chmod +x "$S25/build/tools/doxygen"
cat > "$S25/build/docs/Doxyfile.docs" <<EOF
PROJECT_NAME           = kdsoap
OUTPUT_DIRECTORY       = $S25/build/docs
INPUT                  = $S25/src/docs
RECURSIVE              = YES
EOF
cat > "$S25/build/build.ninja" <<EOF
rule doxy
  command = cd $S25/src/docs && $CMAKE_BIN -E make_directory $S25/build/docs/html && $S25/build/tools/doxygen $S25/build/docs/Doxyfile.docs && $CMAKE_BIN -E touch $S25/build/docs/docs.stamp
build docs/docs.stamp: doxy
EOF
drive "$NN" "$TASK" "$S25/build" docs/docs.stamp > "$D/doxyout.json" \
  || { echo "NO ANSWER: the doxygen-output drive did not emit a derivation"; tail -3 "$S25/build/drive.err"; exit 2; }
O_STAMP=$(carries "$D/doxyout.json" 'docs/docs\.stamp')
O_HTML=$(carries_outputs "$D/doxyout.json" 'docs/html')
echo "doxygen-output stamp(floor)=$O_STAMP html=$O_HTML"
S25B=$D/doxyoutno; mkdir -p "$S25B/build/docs" "$S25B/build/tools" "$S25B/src/docs"
printf '#!/bin/sh\nmkdir -p "$3"\n' > "$S25B/build/tools/doxygen"; chmod +x "$S25B/build/tools/doxygen"
cat > "$S25B/build/docs/Doxyfile.docs" <<EOF
PROJECT_NAME           = kdsoap
OUTPUT_DIRECTORY       = $S25B/build/docs
GENERATE_HTML          = NO
INPUT                  = $S25B/src/docs
EOF
cat > "$S25B/build/build.ninja" <<EOF
rule doxy
  command = cd $S25B/src/docs && $CMAKE_BIN -E make_directory $S25B/build/docs/man && $S25B/build/tools/doxygen $S25B/build/docs/Doxyfile.docs && $CMAKE_BIN -E touch $S25B/build/docs/docs.stamp
build docs/docs.stamp: doxy
EOF
drive "$NN" "$TASK" "$S25B/build" docs/docs.stamp > "$D/doxyout.no.json" \
  || { echo "NO ANSWER: the html-off drive did not emit a derivation"; tail -3 "$S25B/build/drive.err"; exit 2; }
O_NOHTML=$(carries "$D/doxyout.no.json" 'docs/html')
echo "doxygen-output html-off control=$O_NOHTML (must be 0)"

# ---- THE PIN CONTROLS: the same fixtures against the pin's driver -------
if [ -x "$PIN_DRV/bin/nix-ninja" ] && [ -x "$PIN_TASK/bin/nix-ninja-task" ]; then
  drive "$PIN_DRV" "$PIN_TASK" "$S1/build" nvme_wrap.c > "$D/swig.pin.json" 2>/dev/null || true
  drive "$PIN_DRV" "$PIN_TASK" "$S26/build" gen/top_generated.h > "$D/flatc.pin.json" 2>/dev/null || true
  F_PIN=$(carries_inputs "$D/flatc.pin.json" 'common/inc.fbs')
  F_PIN_NESTED=$(carries_inputs "$D/flatc.pin.json" 'common/nested.fbs')
  [ -s "$D/flatc.pin.json" ] || { echo "NO ANSWER: the pin emitted no derivation for the flatc fixture, so its 0 is not a reading"; exit 2; }
  drive "$PIN_DRV" "$PIN_TASK" "$S2/build" out.xml > "$D/its.pin.json" 2>/dev/null || true
  drive "$PIN_DRV" "$PIN_TASK" "$S3/build" upb/util/def_to_proto_test.upbdefs.h > "$D/proto.pin.json" 2>/dev/null || true
  drive "$PIN_DRV" "$PIN_TASK" "$S8/build" out.gir > "$D/girinc.pin.json" 2>/dev/null || true
  H_PIN=$(carries "$D/girinc.pin.json" 'thumb\.h')
  drive "$PIN_DRV" "$PIN_TASK" "$S9/build" yadif.o > "$D/nasm.pin.json" 2>/dev/null || true
  N_PIN=$(carries "$D/nasm.pin.json" 'x86inc\.asm')
  drive "$PIN_DRV" "$PIN_TASK" "$S11/src/build" lib/pk.vapi > "$D/vapigen.pin.json" 2>/dev/null || true
  V_PIN=$(carries "$D/vapigen.pin.json" 'Pk-1\.0\.metadata')
  drive "$PIN_DRV" "$PIN_TASK" "$S12/build" qobject.o > "$D/mocinc.pin.json" 2>/dev/null || true
  M_PIN=$(carries "$D/mocinc.pin.json" 'moc_qobject\.cpp[^.]')
  drive "$PIN_DRV" "$PIN_TASK" "$S14/build" lib/A-1.0.typelib > "$D/girinc2.pin.json" 2>/dev/null || true
  T_PIN=$(carries "$D/girinc2.pin.json" 'lib/B-1\.0\.gir')
  drive "$PIN_DRV" "$PIN_TASK" "$S15/build" tests/t > "$D/pathdef.pin.json" 2>/dev/null || true
  D_PIN=$(carries "$D/pathdef.pin.json" 'testdata/scRGB-v2\.icc')
  drive "$PIN_DRV" "$PIN_TASK" "$S16/build" doc/X-doc > "$D/gidoc.pin.json" 2>/dev/null || true
  G_PIN=$(carries "$D/gidoc.pin.json" 'doc/logo\.svg')
  drive "$PIN_DRV" "$PIN_TASK" "$S17/build" doc/html > "$D/doxy.pin.json" 2>/dev/null || true
  X_PIN=$(carries "$D/doxy.pin.json" 'doc/misc/header\.html')
  drive "$PIN_DRV" "$PIN_TASK" "$S18/build" src/tls/certonly/P_autogen/timestamp > "$D/autogen.pin.json" 2>/dev/null || true
  A_PIN=$(carries "$D/autogen.pin.json" 'tls/shared/asn_p\.h')
  drive "$PIN_DRV" "$PIN_TASK" "$S18B/build" src/core/Q_autogen/timestamp > "$D/autogen2.pin.json" 2>/dev/null || true
  B_PIN=$(carries "$D/autogen2.pin.json" 'Q_autogen/include/moc_q\.cpp')
  # ARM 19's control: the pin's driver must NOT carry the composed module or
  # the vars file; the helper is the floor both sides carry.
  drive "$PIN_DRV" "$PIN_TASK" "$S19/build" opencv4.pc > "$D/cmkinc.pin.json" 2>/dev/null || true
  C_PIN_MODULE=$(carries "$D/cmkinc.pin.json" 'cmake/OpenCVUtils\.cmake')
  C_PIN_VARS=$(carries "$D/cmkinc.pin.json" 'vars/EnableModeVars\.cmake')
  # ARM 20's control: the pin's driver must NOT put xsltproc's bin on the
  # task's PATH, and the reader is absent there by construction. The
  # fixture puts the tool on the drive's PATH the way the subject does,
  # so the pin control is the reader's half, not the environment's.
  ( cd "$S20/build"
      PATH="$PIN_DRV/bin:$PIN_TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin:$CMAKE_BIN_DIR:$XS/bin" \
      NIX_NINJA_TASK="$PIN_TASK/bin/nix-ninja-task" \
      NIX_CONFIG="experimental-features = $FEAT" \
      NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
      "$PIN_DRV/bin/nix-ninja" -t drv out.xml > "$D/s2d.pin.json" 2>/dev/null
    ) || true
  S_XSLT_PIN=$(python3 -c "
import json,sys
try: d=json.load(open('$D/s2d.pin.json'))
except Exception: print(0); sys.exit()
print(int(any('$XS_NAME/bin' in p for v in d.values() for p in v['env'].get('PATH','').split(':'))))")
    ( cd "$S20CTL/build"
      PATH="$PIN_DRV/bin:$PIN_TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin:$CMAKE_BIN_DIR:$XS/bin" \
      NIX_NINJA_TASK="$PIN_TASK/bin/nix-ninja-task" \
      NIX_CONFIG="experimental-features = $FEAT" \
      NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
      "$PIN_DRV/bin/nix-ninja" -t drv out.xml > "$D/s2dctl.pin.json" 2>/dev/null
    ) || true
  S_CTL_XSLT_PIN=$(python3 -c "
import json,sys
try: d=json.load(open('$D/s2dctl.pin.json'))
except Exception: print(0); sys.exit()
print(int(any('$XS_NAME/bin' in p for v in d.values() for p in v['env'].get('PATH','').split(':'))))")
  # ARM 21's control: the pin's driver must NOT carry the `.vapi` in
  # NIX_NINJA_OUTPUTS, and must NOT stamp the declared name.
  drive "$PIN_DRV" "$PIN_TASK" "$S21/build" bindings/vala/AyatanaAppIndicator3-0.1.vala > "$D/vapigen2.pin.json" 2>/dev/null || true
  AV_PIN_OUTS=$(carries "$D/vapigen2.pin.json" 'ayatana-appindicator3-0.1\.vapi')
  AV_PIN_TOUCH=$(carries "$D/vapigen2.pin.json" "touch '[^']*AyatanaAppIndicator3-0[.]1[.]vala'")
  # ARM 22's control: the pin carries neither the climbed file nor the
  # relative one; the no-climb control stays make-dir-free here too.
  drive "$PIN_DRV" "$PIN_TASK" "$S22/build" icons/qrc_app.cpp > "$D/qrc.pin.json" 2>/dev/null || true
  Q_PIN_CLIMB=$(carries "$D/qrc.pin.json" 'shared/img/back\.png')
  Q_PIN_REL=$(carries "$D/qrc.pin.json" 'icons/res\.dat')
  ( cd "$S22/build"
    PATH="$PIN_DRV/bin:$PIN_TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin" \
    NIX_NINJA_TASK="$PIN_TASK/bin/nix-ninja-task" \
    NIX_CONFIG="experimental-features = $FEAT" \
    NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
    "$PIN_DRV/bin/nix-ninja" -f build2.ninja -t drv icons/qrc_plain.cpp > "$D/qrc.ctl.pin.json" 2>/dev/null
  ) || true
  Q_PIN_CTL_DIRS=$(carries "$D/qrc.ctl.pin.json" 'NIX_NINJA_MAKE_DIRS')
  # ARM 27's pin control: the pin carries no produced grd member either.
  drive "$PIN_DRV" "$PIN_TASK" "$S27/build" out.txt > "$D/grddefer.pin.json" 2>/dev/null || true
  GD_PIN=$(carries_inputs "$D/grddefer.pin.json" 'member.dat')
  [ -s "$D/grddefer.pin.json" ] || { echo "NO ANSWER: the pin emitted no derivation for the grd-defer fixture, so its 0 is not a reading"; exit 2; }
  # ARM 22b's controls. The pin runs NO 22b drive: its driver predates the
  # tool handling this fixture needs (an absolute tool under a `cd`
  # prologue fails there with `Failed to find ../tools/rcc: cannot find
  # binary path`, measured, unrelated to qrc), so a pin drive can never
  # succeed-but-silent here and scoring it reads a tool gap as the class.
  # The defect model is HEAD vs aaefd48 instead: the reader is present in
  # both, the deferral only in HEAD.
  # Undeclared at aaefd48: emits WITHOUT the member (the reader is present
  # and the deferral is not, so silence here isolates the deferral as the
  # delta). Failed drive: NO ANSWER, never silence.
  drive "$AAE_DRV" "$AAE_TASK" "$S22B/build" icons/qrc_app.cpp > "$D/qrcdefer.aae.json" || { echo "NO ANSWER: the aaefd48 undeclared drive failed"; tail -3 "$S22B/build/drive.err"; exit 2; }
  QB_AAE_MISS=$(carries "$D/qrcdefer.aae.json" 'icons/gen\.dat')
  # Declared twin at aaefd48: must carry. The generic route is live where
  # the subject runs, which is what makes the undeclared half's silence
  # the defect rather than a broken fixture.
  ( cd "$S22B/build"
    PATH="$AAE_DRV/bin:$AAE_TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin" \
    NIX_NINJA_TASK="$AAE_TASK/bin/nix-ninja-task" \
    NIX_CONFIG="experimental-features = $FEAT" \
    NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
    "$AAE_DRV/bin/nix-ninja" -f build2.ninja -t drv icons/qrc_plain.cpp > "$D/qrcdefer.twinaae.json" 2>"$S22B/build/drive.err"
  ) || { echo "NO ANSWER: the aaefd48 declared-twin drive failed"; tail -3 "$S22B/build/drive.err"; exit 2; }
  QB_AAE_TWIN=$(carries "$D/qrcdefer.twinaae.json" 'icons/gen\.dat')
  # ARM 27's control: the candidate loop is present at aaefd48 and the
  # deferral is not, so the same drive emits WITHOUT the member there. A
  # failed drive is NO ANSWER, never silence.
  drive "$AAE_DRV" "$AAE_TASK" "$S27/build" out.txt > "$D/grddefer.aae.json" 2>/dev/null || {
    echo "NO ANSWER: the aaefd48 grd-defer drive failed"; tail -3 "$S27/build/drive.err"; exit 2; }
  GD_AAE=$(carries_inputs "$D/grddefer.aae.json" 'member.dat')
  [ -s "$D/grddefer.aae.json" ] || { echo "NO ANSWER: the aaefd48 grd-defer drive emitted nothing, so its 0 is not a reading"; exit 2; }
  # ARM 23's control: the pin carries the absolute FILES through the
  # generic absolute-file route but NOT the directory (an absolute DIR is
  # refused there by construction) - the directory is the reader's subject.
  drive "$PIN_DRV" "$PIN_TASK" "$S23/build" out/stamp > "$D/cmkcopy.pin.json" 2>/dev/null || true
  K_PIN_LIB=$(carries_inputs "$D/cmkcopy.pin.json" 'cef/Release/libcef.so')
  K_PIN_LOC=$(carries_inputs "$D/cmkcopy.pin.json" 'Resources/locales/en.pak')
  # ARM 24's control: the pin carries no source_dir tree.
  drive "$PIN_DRV" "$PIN_TASK" "$S24/build" dep-prefix/src/dep-stamp/dep-configure > "$D/epsrc.pin.json" 2>/dev/null || true
  E_PIN_SRC=$(carries "$D/epsrc.pin.json" 'src/dep/CMakeLists')
  # And no url-method tree: the pin predates 6e2ccd3's rebase, so the `-S`
  # climb resolved off the tree there and nothing carried it.
  drive "$PIN_DRV" "$PIN_TASK" "$S24B/build" dep-prefix/src/dep-stamp/dep-configure > "$D/epsrc.dl.pin.json" 2>/dev/null || true
  E_PIN_DL=$(carries "$D/epsrc.dl.pin.json" 'src/dep/CMakeLists')
  [ -s "$D/epsrc.dl.pin.json" ] || { echo "NO ANSWER: the pin emitted no derivation for the url-method twin, so its 0 is not a reading"; exit 2; }
  echo "ep-sourcedir url-method at the pin=$E_PIN_DL (must be 0)"
  # ARM 25's control: the pin carries the stamp but no html tree.
  drive "$PIN_DRV" "$PIN_TASK" "$S25/build" docs/docs.stamp > "$D/doxyout.pin.json" 2>/dev/null || true
  O_PIN_STAMP=$(carries "$D/doxyout.pin.json" 'docs/docs\.stamp')
  O_PIN_HTML=$(carries_outputs "$D/doxyout.pin.json" 'docs/html')
  ( cd "$S10/build"
    PATH="$PIN_DRV/bin:$PIN_TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin:$CMAKE_BIN_DIR" \
    NIX_NINJA_TASK="$PIN_TASK/bin/nix-ninja-task" \
    NIX_CONFIG="experimental-features = $FEAT" \
    NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
    "$PIN_DRV/bin/nix-ninja" -t drv lib/t > "$D/soname.pin.json" 2>/dev/null
  ) || true
  L_PIN=$(carries "$D/soname.pin.json" 'lib/libm\.so\.2[^.]')
  S_PIN=$(carries "$D/swig.pin.json" 'types\.h')
  I_PIN=$(carries "$D/its.pin.json" 'colord\.its')
  P_PIN=$(carries "$D/proto.pin.json" 'def_to_proto_public_import_test\.proto')
  # ARM 7's control: the pin's driver must NOT carry the compiler for the
  # same fixture, or the fixture does not model the class.
  ( cd "$S7/build"
    PATH="$PIN_DRV/bin:$PIN_TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin" \
    NIX_NINJA_TASK="$PIN_TASK/bin/nix-ninja-task" \
    NIX_CONFIG="experimental-features = $FEAT" \
    NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
    "$PIN_DRV/bin/nix-ninja" -t drv out.gir > "$D/gircc.pin.json" 2>/dev/null
  ) || true
  G_CC_PIN=$(python3 -c "
import json
try: d=json.load(open('$D/gircc.pin.json'))
except Exception: print(0); sys.exit()
n=0
for v in d.values():
    ins=v.get('inputs',{})
    allins=' '.join(list(ins.get('srcs',[]))+list(ins.get('drvs',{}).keys()))
    onpath='gcc-wrapper' in v.get('env',{}).get('PATH','')
    if ('gcc-wrapper' in allins or 'gcc-16' in allins) and onpath: n+=1
print(n)")
  # ARM 4's control: the same fixture against the pin's driver, same PATH and
  # XDG_DATA_DIRS. A pin that carries either arm means the fixture does not
  # model the class (or the control rev is not old enough).
  ( cd "$S4/build"
    PATH="$PIN_DRV/bin:$PIN_TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin:$CMAKE_BIN_DIR" \
    XDG_DATA_DIRS="$NINJA_STORE/share" \
    NIX_NINJA_TASK="$PIN_TASK/bin/nix-ninja-task" \
    NIX_CONFIG="experimental-features = $FEAT" \
    NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
    "$PIN_DRV/bin/nix-ninja" -t drv dep-prefix/src/dep-stamp/dep-configure > "$D/ep.pin.json" 2>/dev/null
  ) || true
  E_NINJA_PIN=$(python3 -c "
import json,sys
try: d=json.load(open('$D/ep.pin.json'))
except Exception: print(0); sys.exit()
print(int(any('ninja-1.13' in p for v in d.values() for p in v['env'].get('PATH','').split(':'))))")
  E_BINDIR_PIN=$(python3 -c "
import json,sys
try: d=json.load(open('$D/ep.pin.json'))
except Exception: print(0); sys.exit()
print(int(any(l.split(':')[1]=='deps/build/dep' for v in d.values() for l in v['env'].get('NIX_NINJA_OUTPUTS','').split(chr(10)) if l)))")
  # ARM 5's control: the SAME non-step regenerate fixture against the pin's
  # driver. The control rev (37672a4 default) long predates the reader, so it
  # declares nothing either - the CONTROL THAT MATTERS for this arm is not a
  # revision but `425778a`'s parent, which the gate cannot build by default.
  # So this arm asserts only the HEAD direction and says so rather than
  # claiming a control it does not have.
else
  echo "NO ANSWER: the pin's binaries are not resident, so the control cannot run"; exit 2
fi

echo "swig  types.h   here=$S_HERE  pin=$S_PIN   (edge declares nvme.i: $S_DECL refs)"
echo "its   colord.its here=$I_HERE  pin=$I_PIN"
echo "proto plain=$P_REG public=$P_PUB option=$P_OPT  pin=$P_PIN"
echo "ep    ninja-on-PATH=$E_NINJA bindir-declared=$E_BINDIR  pin: ninja=${E_NINJA_PIN:-?} bindir=${E_BINDIR_PIN:-?}"
echo "regen non-step edge declares a tree: $E_REGEN (must be 0)"
echo "vapigen metadata here=${V_MD:-?} pin=${V_PIN:-?}  (gir: ${V_GIR:-?})"
echo "soname libm.so.2 here=${L_SONAME:-?} pin=${L_PIN:-?} direct=${L_DIRECT:-?} (must be 0)  (real libm.so.2.17: ${L_REAL:-?}, direct ${L_DIRECT_REAL:-?})"
echo "nasm  x86inc.asm here=${N_INC:-?} pin=${N_PIN:-?}"
echo "girinc thumb.h here=${H_THUMB:-?} pin=${H_PIN:-?}  (listed bg.h: ${H_LISTED:-?})"
echo "mkenums template=$M_TMPL header=$M_HDR  (cd above the build dir)"
echo "cmkinc script=$C_SCRIPT helper=$C_HELPER module=$C_MODULE vars=$C_VARS  (pin: module=${C_PIN_MODULE:-?} vars=${C_PIN_VARS:-?})"
echo "s2d   script=$S_SCRIPT xslt-src=$S_XSLT_SRC xslt-path=$S_XSLT_PATH  (ctl: script=$S_CTL_SCRIPT xslt-src=$S_CTL_XSLT_SRC xslt-path=$S_CTL_XSLT_PATH; pin: xslt=${S_XSLT_PIN:-?} ctl=${S_CTL_XSLT_PIN:-?})"
echo "vapi  out=$AV_VAPI_OUT touch=$AV_TOUCH vala-out=$AV_VALA_OUT  (floor gir=$AV_GIR; pin: outs=${AV_PIN_OUTS:-?} touch=${AV_PIN_TOUCH:-?})"
echo "flatc include here=${F_HERE:-?} nested=${F_NESTED:-?} declared=${F_DECL:-?}  (pin include=${F_PIN:-?} nested=${F_PIN_NESTED:-?})"

# A DECLARED FILE MISSING HERE MEANS THE FIXTURE IS BROKEN, not that the reader
# is: the edge names it, so it must be carried whatever the reader does.
if [ "${S_DECL:-0}" -lt 1 ]; then
  echo "NO ANSWER: the swig fixture does not carry its OWN declared .i, so it is broken"; exit 2
fi
if [ "${M_TMPL:-0}" -lt 1 ] || [ "${M_HDR:-0}" -lt 1 ]; then
  echo "FAIL: a glib-mkenums name did not reach the emitted derivation (template=${M_TMPL:-?}, header=${M_HDR:-?}) - the cd above the build dir was dropped and the reader resolved against the wrong directory"
  exit 1
fi
if [ "$S_HERE" -lt 1 ] || [ "$I_HERE" -lt 1 ] || [ "${P_PUB:-0}" -lt 1 ] \
   || [ "${P_REG:-0}" -lt 1 ] || [ "${P_OPT:-0}" -lt 1 ] || [ "${F_HERE:-0}" -lt 1 ] \
   || [ "${F_NESTED:-0}" -lt 1 ] || [ "${GD_MISS:-0}" -lt 1 ]; then
  echo "FAIL: a reader's output did not reach the emitted derivation"
  exit 1
fi
if [ "$S_PIN" -ge 1 ] || [ "$I_PIN" -ge 1 ] || [ "${P_PIN:-0}" -ge 1 ] \
   || [ "${F_PIN:-0}" -ge 1 ] || [ "${F_PIN_NESTED:-0}" -ge 1 ] || [ "${GD_PIN:-0}" -ge 1 ]; then
  echo "FAIL: the pin already carries a target, so the fixture does not model the class"
  exit 1
fi
# ARM 26: the edge declares its own top schema whatever the reader does, so a
# zero there is a broken fixture rather than a reader finding.
if [ "${F_DECL:-0}" -lt 1 ]; then
  echo "NO ANSWER: the flatc fixture does not carry its OWN declared top.fbs, so it is broken"; exit 2
fi
# ARM 27: the deferral is the only route, so the defer line is scored; and the
# declared manifest is the floor that makes the member's absence the defect.
if [ "${GD_DEFER:-0}" -lt 1 ]; then
  echo "FAIL: the grd candidate did not defer on its produced member - the deferral arm is gone and grit dies FileNotFoundError"
  exit 1
fi
if [ "${GD_DECL:-0}" -lt 1 ]; then
  echo "NO ANSWER: the grd-defer fixture does not carry its OWN declared app.grd, so it is broken"; exit 2
fi
if [ "${GD_AAE:-1}" -ge 1 ]; then
  echo "FAIL: aaefd48 already carries the produced grd member, so the fixture does not isolate the deferral"
  exit 1
fi
# ARM 4: both ExternalProject arms must fire here and NEITHER at the pin.
if [ "${E_NINJA:-0}" -lt 1 ] || [ "${E_BINDIR:-0}" -lt 1 ]; then
  echo "FAIL: an ExternalProject arm did not fire (ninja-on-PATH=${E_NINJA:-?}, bindir-declared=${E_BINDIR:-?})"
  exit 1
fi
if [ "${E_NINJA_PIN:-0}" -ge 1 ] || [ "${E_BINDIR_PIN:-0}" -ge 1 ]; then
  echo "FAIL: the pin already carries an ExternalProject arm, so the fixture does not model the class"
  exit 1
fi
# ARM 5: a non-step edge carrying cmake's regenerate pair must declare NO tree.
if [ "${E_REGEN:-1}" != 0 ]; then
  echo "FAIL: an ungated reader declared a tree on a NON-ExternalProject edge (got $E_REGEN, want 0) - this is the class that shipped broken twice"
  exit 1
fi
# ARM 7: the scanner's compiler must be carried HERE and NOT at the pin.
if [ "${G_CC:-0}" -lt 1 ]; then
  echo "FAIL: a g-ir-scanner edge did not carry the compiler it shells out to unnamed (got ${G_CC:-?}) - the task will die FileNotFoundError 'gcc'"
  exit 1
fi
if [ "${G_CC_PIN:-0}" -ge 1 ]; then
  echo "FAIL: the pin already carries the compiler for the scanner fixture, so the fixture does not model the class"
  exit 1
fi
# ARM 8: the listed header is the fixture's own floor; the included one is the
# subject, carried HERE and NOT at the pin.
if [ "${H_LISTED:-0}" -lt 1 ]; then
  echo "NO ANSWER: the gir filelist fixture does not carry its OWN listed header, so it is broken"; exit 2
fi
if [ "${H_THUMB:-0}" -lt 1 ]; then
  echo "FAIL: a header a scanned gir source includes did not reach the emitted derivation - the task will die No such file or directory"
  exit 1
fi
if [ "${H_PIN:-0}" -ge 1 ]; then
  echo "FAIL: the pin already carries the gir header closure, so the fixture does not model the class"
  exit 1
fi
# ARM 9: the assembler include, HERE and NOT at the pin.
if [ "${N_INC:-0}" -lt 1 ]; then
  echo "FAIL: an assembler %include did not reach the emitted derivation - nasm will die unable to open include file"
  exit 1
fi
if [ "${N_PIN:-0}" -ge 1 ]; then
  echo "FAIL: the pin already carries the assembler include, so the fixture does not model the class"
  exit 1
fi
# ARM 10: the real library is the floor; the soname link is the subject.
if [ "${L_REAL:-0}" -lt 1 ]; then
  echo "NO ANSWER: the soname fixture does not carry its OWN library, so it is broken"; exit 2
fi
if [ "${L_SONAME:-0}" -lt 1 ]; then
  echo "FAIL: a soname link made by a sibling edge did not reach the link task - ld will not find the DT_NEEDED library"
  exit 1
fi
if [ "${L_DIRECT_REAL:-0}" -lt 1 ]; then
  echo "NO ANSWER: the direct soname fixture does not carry its own library"; exit 2
fi
if [ "${L_DIRECT:-1}" != 0 ]; then
  echo "FAIL: a task handed the library directly gained its soname link (got $L_DIRECT) - every such banked link would re-key"
  exit 1
fi
if [ "${L_PIN:-0}" -ge 1 ]; then
  echo "FAIL: the pin already carries the soname link, so the fixture does not model the class"
  exit 1
fi
# ARM 11: the gir is the floor; the metadata is the subject.
if [ "${V_GIR:-0}" -lt 1 ]; then
  echo "NO ANSWER: the vapigen fixture does not carry its OWN gir, so it is broken"; exit 2
fi
if [ "${V_MD:-0}" -lt 1 ]; then
  echo "FAIL: vapigen's namespace metadata did not reach the emitted derivation - its renames never apply"
  exit 1
fi
if [ "${V_PIN:-0}" -ge 1 ]; then
  echo "FAIL: the pin already carries the vapigen metadata, so the fixture does not model the class"
  exit 1
fi
# ARM 12: the json co-output is the floor (the phony route works at all); the
# included .cpp is the subject; the un-included sibling is the control.
if [ "${M_JSON:-0}" -lt 1 ]; then
  echo "NO ANSWER: the moc fixture does not carry the .json the phony names, so the phony route is broken"; exit 2
fi
if [ "${M_INC:-0}" -lt 1 ]; then
  echo "FAIL: the generated source the TU includes did not reach the emitted derivation (dropped as a compile artifact)"
  exit 1
fi
if [ "${M_OTHER:-0}" -ge 1 ]; then
  echo "FAIL: a generated source the TU does NOT include was carried, so the phony filter no longer holds"
  exit 1
fi
if [ "${M_PIN:-0}" -ge 1 ]; then
  echo "FAIL: the pin already carries the included generated source, so the fixture does not model the class"
  exit 1
fi
# ARM 13: the msgfmt task carries the variable AND its store path; the swig
# task in the same environment carries neither.
if [ "${G_ENV:-0}" -lt 1 ] || [ "${G_SRC:-0}" -lt 1 ]; then
  echo "FAIL: GETTEXTDATADIRS (env=$G_ENV) or its store path (src=$G_SRC) did not reach the msgfmt task"
  exit 1
fi
if [ "${G_CTL:-0}" -ge 1 ]; then
  echo "FAIL: GETTEXTDATADIRS reached a task that does not run msgfmt, so the forward is not command-scoped"
  exit 1
fi
# ARM 14: the typelib's own gir is the floor; the included gir is the subject.
if [ "${T_A:-0}" -lt 1 ]; then
  echo "NO ANSWER: the typelib fixture does not carry its OWN gir, so it is broken"; exit 2
fi
if [ "${T_B:-0}" -lt 1 ]; then
  echo "FAIL: the gir included by meson's RELATIVE spelling did not reach the emitted derivation"
  exit 1
fi
if [ "${T_C:-0}" -lt 1 ]; then
  echo "FAIL: the gir included by an ABSOLUTE spelling did not reach the emitted derivation"
  exit 1
fi
if [ "${T_PIN:-0}" -ge 1 ]; then
  echo "FAIL: the pin already carries the included gir, so the fixture does not model the class"
  exit 1
fi
# ARM 15: the link that runs the binary carries the directory; the compile
# that defines it does not; the pin carries nothing.
if [ "${D_LINK:-0}" -lt 1 ]; then
  echo "FAIL: the directory a define bakes into the binary did not reach the link task that runs it"
  exit 1
fi
if [ "${D_CTL:-0}" -ge 1 ]; then
  echo "FAIL: the compile edge that only DEFINES the path carried the directory, so the carry is not scoped to the runner"
  exit 1
fi
if [ "${D_PIN:-0}" -ge 1 ]; then
  echo "FAIL: the pin already carries the defined directory, so the fixture does not model the class"
  exit 1
fi
# ARM 16: the declared page is the floor; the image and the urlmap are the
# subject; the pin carries neither.
if [ "${G_PAGE:-0}" -lt 1 ]; then
  echo "NO ANSWER: the gi-docgen fixture does not carry its DECLARED page, so it is broken"; exit 2
fi
if [ "${G_IMG:-0}" -lt 1 ] || [ "${G_MAP:-0}" -lt 1 ]; then
  echo "FAIL: a content file the gi-docgen TOML names did not reach the emitted derivation (image=$G_IMG urlmap=$G_MAP)"
  exit 1
fi
if [ "${G_PIN:-0}" -ge 1 ]; then
  echo "FAIL: the pin already carries the TOML-named image, so the fixture does not model the class"
  exit 1
fi
# ARM 17: the Doxyfile is the floor; the header (file route) and a file under
# the INPUT directory (directory route) are the subject; the pin carries neither.
if [ "${X_CFG:-0}" -lt 1 ]; then
  echo "NO ANSWER: the doxygen fixture does not carry its DECLARED Doxyfile, so it is broken"; exit 2
fi
if [ "${X_HDR:-0}" -lt 1 ] || [ "${X_INC:-0}" -lt 1 ]; then
  echo "FAIL: a path the Doxyfile names did not reach the emitted derivation (header=$X_HDR include=$X_INC)"
  exit 1
fi
if [ "${X_PIN:-0}" -ge 1 ]; then
  echo "FAIL: the pin already carries the Doxyfile's header, so the fixture does not model the class"
  exit 1
fi
# ARM 18: own.h (directory route) is the floor; the sibling HEADERS file is the
# subject; the pin carries the floor and not the subject.
if [ "${A_OWN:-0}" -lt 1 ]; then
  echo "NO ANSWER: the autogen fixture does not carry own.h by the MOC_INCLUDES route, so it is broken"; exit 2
fi
if [ "${A_SIB:-0}" -lt 1 ]; then
  echo "FAIL: a header the AutogenInfo.json HEADERS list names did not reach the emitted derivation"
  exit 1
fi
if [ "${A_PIN:-0}" -ge 1 ]; then
  echo "FAIL: the pin already carries the sibling header, so the fixture does not model the class"
  exit 1
fi
# ARM 18b: cfg.h (the walk reached the dir) and the plan are the floor; the
# placed moc file and ParseCache.txt beneath the edge's own trees are the
# subject. A pin older than `62424cd` carries the moc file, which is the
# control that the fixture models the class.
if [ "${B_CFG:-0}" -lt 1 ] || [ "${B_PLAN:-0}" -lt 1 ]; then
  echo "FAIL: the second-pass autogen task lost its configure file or its plan (cfg=$B_CFG plan=$B_PLAN)"
  exit 1
fi
if [ "${B_MOC:-0}" -ge 1 ] || [ "${B_CACHE:-0}" -ge 1 ]; then
  echo "FAIL: an autogen edge carries its own earlier output as an input (moc=$B_MOC cache=$B_CACHE) - moc will die EACCES writing it"
  exit 1
fi
# ARM 19: the script and the helper are the floor; the module (composed
# from the helper's `set`) and the vars file (`CMAKE_CURRENT_LIST_DIR`,
# third level) are the subjects, carried HERE and NOT at the pin.
if [ "${C_SCRIPT:-0}" -lt 1 ]; then
  echo "NO ANSWER: the cmake -P fixture does not carry its OWN script, so it is broken"; exit 2
fi
if [ "${C_HELPER:-0}" -lt 1 ]; then
  echo "NO ANSWER: the cmake -P fixture does not carry its declared helper, so it is broken"; exit 2
fi
if [ "${C_MODULE:-0}" -lt 1 ] || [ "${C_VARS:-0}" -lt 1 ]; then
  echo "FAIL: a composed cmake include did not reach the emitted derivation (module=$C_MODULE vars=$C_VARS) - the task will die 'include could not find requested file'"
  exit 1
fi
if [ "${C_PIN_MODULE:-0}" -ge 1 ] || [ "${C_PIN_VARS:-0}" -ge 1 ]; then
  echo "FAIL: the pin already carries the composed cmake include, so the fixture does not model the class"
  exit 1
fi
# ARM 20: the script itself is the floor; xsltproc must be carried HERE
# (input closure AND emitted PATH), and the cat control must carry the
# script but NOT xsltproc; the pin carries xsltproc nowhere.
if [ "${S_SCRIPT:-0}" -lt 1 ]; then
  echo "NO ANSWER: the shell-script fixture does not carry its OWN script, so it is broken"; exit 2
fi
if [ "${S_XSLT_SRC:-0}" -lt 1 ] || [ "${S_XSLT_PATH:-0}" -lt 1 ]; then
  echo "FAIL: a tool a shell script runs did not reach the emitted derivation (src=$S_XSLT_SRC path=$S_XSLT_PATH) - the task will die '<tool>: not found' and exit 0"
  exit 1
fi
if [ "${S_CTL_SCRIPT:-0}" -lt 1 ]; then
  echo "NO ANSWER: the cat control fixture does not carry the script, so it is broken"; exit 2
fi
if [ "${S_CTL_XSLT_SRC:-0}" -ge 1 ] || [ "${S_CTL_XSLT_PATH:-0}" -ge 1 ]; then
  echo "FAIL: a tool a script merely PASSES THROUGH cat reached the derivation, so the reader is not command-scoped"
  exit 1
fi
if [ "${S_XSLT_PIN:-0}" -ge 1 ]; then
  echo "FAIL: the pin already carries the shell-script tool, so the fixture does not model the class"
  exit 1
fi
if [ "${S_CTL_XSLT_PIN:-0}" -ge 1 ]; then
  echo "FAIL: the pin already carries the tool on the cat control, so the fixture does not model the class"
  exit 1
fi
# ARM 21: the `.vapi` is in NIX_NINJA_OUTPUTS and the declared `.vala` is
# stamped; the floor is the declared `.gir` input and the declared `.vala`
# output, both of which carry whatever the reader does.
if [ "${AV_GIR:-0}" -lt 1 ]; then
  echo "NO ANSWER: the vapigen fixture does not carry its OWN gir, so it is broken"; exit 2
fi
if [ "${AV_VALA_OUT:-0}" -lt 1 ]; then
  echo "NO ANSWER: the vapigen fixture does not carry its DECLARED .vala output, so it is broken"; exit 2
fi
if [ "${AV_VAPI_OUT:-0}" -lt 1 ]; then
  echo "FAIL: the .vapi vapigen writes did not reach NIX_NINJA_OUTPUTS - the install will read a file that stays in the sandbox"
  exit 1
fi
if [ "${AV_TOUCH:-0}" -lt 1 ]; then
  echo "FAIL: the declared .vala vapigen never writes was not stamped - the task's written-output check will refuse it"
  exit 1
fi
if [ "${AV_PIN_OUTS:-0}" -ge 1 ] || [ "${AV_PIN_TOUCH:-0}" -ge 1 ]; then
  echo "FAIL: the pin already carries the .vapi or the stamp, so the fixture does not model the class"
  exit 1
fi
# ARM 22: the climbed file and the relative file ride HERE and not at the
# pin; the climb prefixes ride make_dirs; the no-climb control stays
# make-dir-free on both sides (a banked rcc task must not move).
if [ "${Q_CLIMB:-0}" -lt 1 ] || [ "${Q_REL:-0}" -lt 1 ]; then
  echo "FAIL: a .qrc entry did not reach the emitted derivation (climb=$Q_CLIMB relative=$Q_REL)"
  exit 1
fi
if [ "${Q_DIRS:-0}" -lt 1 ]; then
  echo "FAIL: the .qrc climb's intermediate prefixes did not reach make_dirs - rcc will die Cannot find file on the un-normalized path"
  exit 1
fi
if [ "${Q_CTL_DIRS:-0}" -ge 1 ]; then
  echo "FAIL: a climb-free .qrc gained make_dirs (got $Q_CTL_DIRS) - every passing rcc task would re-key"
  exit 1
fi
if [ "${Q_PIN_CLIMB:-0}" -ge 1 ] || [ "${Q_PIN_REL:-0}" -ge 1 ]; then
  echo "FAIL: the pin already carries the qrc entries, so the fixture does not model the class"
  exit 1
fi
# ARM 22b: the undeclared member rides HERE through deferral and not at
# aaefd48; the declared twin rides at HEAD and at aaefd48 alike (the
# generic route is live where the subject runs, so silence on the
# undeclared half is the defect, not a broken fixture). No pin drive: the
# pin's driver cannot run this fixture's tool shape at all.
if [ "${QB_MISS:-0}" -lt 1 ]; then
  echo "FAIL: the undeclared member did not reach the emitted derivation - the consumer carries nothing and rcc dies on the absent member"
  exit 1
fi
if [ "${QB_DEFER:-0}" -lt 1 ]; then
  echo "FAIL: the member arrived without deferral - for this drive deferral is the only route, so a carry without it means the fixture bound another way"
  exit 1
fi
if [ "${QB_BIND:-0}" -lt 1 ]; then
  echo "NO ANSWER: the declared generated-member twin carries nothing, so the bind half is broken"; exit 2
fi
if [ "${QB_AAE_TWIN:-0}" -lt 1 ]; then
  echo "NO ANSWER: the declared twin carries nothing at aaefd48, so the generic route the control assumes is gone"; exit 2
fi
if [ "${QB_AAE_MISS:-1}" -ge 1 ]; then
  echo "FAIL: aaefd48 already binds the undeclared member, so the deferral is not the delta"
  exit 1
fi
# ARM 22c: the self-output edge emits with no deferral.
if [ "${QS_OUT:-0}" -lt 1 ]; then
  echo "NO ANSWER: the self-output qrc drive emitted no outputs key, so the fixture is broken"; exit 2
fi
if [ "${QS_DEFER:-1}" != 0 ]; then
  echo "FAIL: the edge deferred on its own qrc output - the self-output guard is gone"
  exit 1
fi
# ARM 22d: the generated qrc binds HERE through deferral; its source member
# is not asserted (open shape, unreadable at scan time).
if [ "${QD_DEFER:-0}" -lt 1 ]; then
  echo "FAIL: the generated qrc arrived without deferral - for this drive deferral is the only route"
  exit 1
fi
if [ "${QD_QRC:-0}" -lt 1 ]; then
  echo "FAIL: the generated qrc did not reach the emitted derivation's inputs - the push half is broken"
  exit 1
fi
# ARM 23: the copy DIRECTORY rides HERE and not at the pin; the absolute
# files and the declared input are the floor (they ride the generic
# absolute-file route on both sides, which is why the reader takes only
# directories). The pin carrying the files is the fixture's own proof the
# generic route is live.
if [ "${K_IN:-0}" -lt 1 ]; then
  echo "NO ANSWER: the cmake-copy fixture does not carry its OWN declared input, so it is broken"; exit 2
fi
if [ "${K_LIB:-0}" -lt 1 ] || [ "${K_DAT:-0}" -lt 1 ]; then
  echo "NO ANSWER: the cmake-copy fixture lost its absolute files, so the generic route it stands on is broken"; exit 2
fi
if [ "${K_LOC:-0}" -lt 1 ]; then
  echo "FAIL: a cmake -E copy_directory source did not reach the emitted derivation (locales=$K_LOC)"
  exit 1
fi
if [ "${K_PIN_LIB:-0}" -lt 1 ]; then
  echo "NO ANSWER: the pin lost the absolute files, so the generic route the control assumes is gone"; exit 2
fi
if [ "${K_PIN_LOC:-0}" -ge 1 ]; then
  echo "FAIL: the pin already carries the copy directory, so the fixture does not model the class"
  exit 1
fi
# ARM 24: the source_dir tree rides HERE and not at the pin; the
# url-method twin's out-of-tree SOURCE_DIR rides the directory-argument
# reader, since no extract step declares it.
if [ "${E_SRC:-0}" -lt 1 ]; then
  echo "FAIL: the ExternalProject source_dir tree did not reach the emitted derivation - cmake -S will die on the absent dir"
  exit 1
fi
if [ "${E_DL:-0}" -lt 1 ]; then
  echo "FAIL: the url-method configure's out-of-tree SOURCE_DIR did not reach the emitted derivation - cmake -S will die on the absent dir"
  exit 1
fi
if [ "${E_PIN_SRC:-0}" -ge 1 ]; then
  echo "FAIL: the pin already carries the source_dir tree, so the fixture does not model the class"
  exit 1
fi
if [ "${E_PIN_DL:-0}" -ge 1 ]; then
  echo "FAIL: the pin already carries the url-method tree, so the fixture does not model the class"
  exit 1
fi
# ARM 25: the stamp is the floor on both sides; the html tree rides HERE,
# not on the html-off twin and not at the pin.
if [ "${O_STAMP:-0}" -lt 1 ]; then
  echo "NO ANSWER: the doxygen fixture does not carry its OWN stamp, so it is broken"; exit 2
fi
if [ "${O_HTML:-0}" -lt 1 ]; then
  echo "FAIL: the html tree a doxygen stamp edge writes did not reach NIX_NINJA_OUTPUTS - install will read a file that stays in the sandbox"
  exit 1
fi
if [ "${O_NOHTML:-1}" != 0 ]; then
  echo "FAIL: the GENERATE_HTML=NO edge gained an html output (got $O_NOHTML) - the reader does not answer the config"
  exit 1
fi
if [ "${O_PIN_HTML:-0}" -ge 1 ]; then
  echo "FAIL: the pin already carries the doxygen html tree, so the fixture does not model the class"
  exit 1
fi
echo "PASS: every reader's targets reach the emitted derivation, both ExternalProject arms fire, no non-step edge declares a tree, the scanner carries its compiler, composed cmake includes ride the task, a script's tools land on PATH, the vapigen .vapi is declared, qrc climbs ride make_dirs and on-disk generated members defer-then-bind with no self-deferral, generated qrcs bind through deferral, the copy directory and the EP source_dir are carried, and the doxygen html tree is declared"
