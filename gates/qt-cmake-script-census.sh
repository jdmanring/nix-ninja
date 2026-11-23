#!/usr/bin/env bash
# DOES A QT RELEASE RUN A `cmake -P` SCRIPT NOBODY HAS REVIEWED?
#
# A Qt build edge that runs `cmake -P <script>` can write a file its edge does
# not declare, and a sandboxed task keeps only what the driver records. Qt
# 6.12's QtFinishPrlFile.cmake did exactly that through a changed interface,
# and qtbase's install pass failed a stage later (fixed in `0cbd22a`).
# Whether a script writes only its declared OUTPUT is a reading of its source,
# so this gate does not decide it. It decides the part a machine can: every
# `-P` CALL SITE it can see in the consumer's Qt modules is on the SITES list
# below, every script those sites run has a disposition on the REVIEWED list,
# and anything new, moved or gone on a Qt bump is a FAIL naming it.
#
# WHAT IT READS. The closure of a consumer target derivation (argument or
# NN_QT_TARGET), each `qt<name>-<ver>.drv` in it, that drv's `src` and its
# `patches`. From the source, only `*.cmake`, `*.cmake.in` and
# `CMakeLists.txt` files (a tarball is extracted to scratch; a store directory
# is read in place); from each patch, the lines it adds to such files. Store
# reads and `nix path-info`/`nix derivation show` only, no build: safe beside
# a round.
#
# A SITE is `<module> <file> <key>`: the file is relative to the module's
# source root (version-free), or `patch:<patch>/<file>` for a line a patch
# adds. The key is the script's file name without `.cmake`, or for a script
# reached through a variable `${var}->Name`, with Name taken from the
# `set(var .../Name.cmake)` lines in the same file. So a reviewed name run
# from a new file is a new site, and a variable pointed at a different script
# is a new key; either FAILs. The SITES list and the scan must be EQUAL: a
# site that disappears also FAILs, so the list cannot rot into a superset.
#
# THE SPELLINGS OF `-P` it reads: a literal path, a variable, an argument on
# the next line, the flag quoted as its own argument (`"-P" "${f}"`), and
# escaped quotes inside generated script text (`-P \"...\"`); whole-line
# comments are skipped. A script argument that is neither a `${var}` nor a
# `*.cmake` path becomes a `?<token>` key and FAILs until reviewed.
#
# WHAT IT DOES NOT SEE: `-P` in files of other names (none is a ninja edge in
# the reviewed release); `-P"file"` with no space, or a `-P` assembled by
# string concatenation (neither occurs in the reviewed release); a variable
# whose `set` is in another file or is a function argument, which stays keyed
# as `${var}` (four such keys, each reviewed below); a patch that splits a
# `-P` across added lines (each added line is read alone).
#
# VERDICTS: PASS (0) sites and scan equal and every key reviewed; FAIL (1) a
# new site, a gone site or an unreviewed key, each named; NO ANSWER (2) no
# target, no Qt module in the closure, a `qt*` derivation that is neither a
# module, its source tarball nor a setup hook, a derivation that cannot be
# shown, or a source, patch or file that cannot be read. The self-control
# runs first and must see each spelling, a repointed variable, a reviewed
# name at a new site, and a patch-added `-P`; a comment must yield nothing.
#
# REVIEWED against Qt 6.12.0 (qtbase, qtdeclarative, the qtwebengine 6.140.0
# source, seven smaller modules that run no `-P` script) and their nixpkgs
# patches, none of which adds a `-P`.
set -u
SCRATCH=${TMPDIR:-/tmp}/qt-cmake-script-census.$$
trap 'rm -rf "$SCRATCH"' EXIT
mkdir -p "$SCRATCH"

# key|disposition. A key moves here only after the script's source is read.
REVIEWED='
QtFinishPrlFile|writes an UNDECLARED final .prl, which the driver records as an output of its edge (undeclared_outputs)
${copy_files_script_path}->Qt6QmlCopyFiles|every qt_add_qml_module: copies qml and resource files into the build dir, an UNDECLARED write the driver does not record; Qt also copies each file at configure time when its source exists, so only a file generated during the build is missed
QtFinishPkgConfigFile|writes only its declared OUTPUT
QtGenerateExtPri|writes only its declared OUTPUT
QtGenerateLibPri|writes only its declared OUTPUT
QtGenerateVersionScript|writes only its declared OUTPUT
QtModuleHeadersCheck|writes only its declared OUTPUT
QtCompressMimeDatabase|writes its declared OUTPUT and a .archive it reads back in the same command
QtCopyFileIfDifferent|a -P script only on Windows below CMake 3.23; callers are Android
QtAndroidSignPackage|Android only
QtSetupAndroid|Android only
${sign_package_script}->QtAndroidSignPackage|Android only
${setup_script}->QtSetupAndroid|Android gradle cache check
${qml_scan_script}->QtAndroidScanQmlImports|Android only
${install_script}->HarmonyOSInstallHapFixture|HarmonyOS hap install fixture
${assemble_sbom}->assemble_sbom${suffix}${multi_config_suffix}|SBOM generation; nixpkgs no-sbom.patch sets QT_GENERATE_SBOM OFF
${reuse_lint_script}|SBOM generation, off as above
${script_wrapper}->run_sbom_source_verify_build_time_${arg_REPO_PROJECT_NAME_LOWERCASE}|SBOM verification, off as above
QtGnSbom|qtwebengine SBOM; qt_webengine_sbom_project_end returns when QT_GENERATE_SBOM is off
${symlinker_script}->QtWebEngineRspSymlinker|static macOS and iOS QtPdf only
QtSynchronizeRepo|static doc-tools build, a separate CMake process, not a ninja edge
qt-cmake-private-install|installed helper, run by tests and users, not a build edge
check_zstd|configure-time probe, not a ninja edge
scriptWrapper|test helper
check_moc_parameters|test
VerifyDefines|test
verify_no_qt_modules|test
verify_signature|test
ensure_pro_file|installed CI file, never run by the build
${test}|RunCMake test driver
${QT_RUN_CMAKE_SCRIPT_PATH}->RunCMakeTest|RunCMake test helper
${wrapper_file}->build_and_test_${testname},build_and_test_module_includes|Qt6CTestMacros add_test wrapper, run by ctest, not a ninja edge
${qmake_wrapper_file}->run_qmake_${testname}|Qt6CTestMacros add_test wrapper, run by ctest, not a ninja edge
${build_tool_wrapper_file}->run_build_${testname}|Qt6CTestMacros add_test wrapper, run by ctest, not a ninja edge
${arg_OUTPUT_FILE}|QtTestHelpers add_test wrapper, run by ctest, not a ninja edge
${benchmark_wrapper_file}->${target}Wrapper$<CONFIG>|QtTestHelpers <target>_benchmark custom target, not in ALL, never built by a package build
${environment_extras}|scanner artefact: a test wrapper template (QtPublicTestHelpers) whose `#!cmake -P` shebang ends its line, so the next non-comment line reads as the token; no edge runs it
Qt6WriteQtConfPart|writes only its declared OUTPUT
Qt6UpdateQmllsIni|removes stale timestamps, writes nothing
ensure_aot_compiled|test
verify_execution|test
QtGnGen|GN generation, a local edge (GN_SCRIPTS), runs on the real tree
QtGnCredits|GN credits, a local edge (GN_SCRIPTS), runs on the real tree
cmake_install|CMake install script, run by the install step, not a sandboxed task
SubmoduleUpdate|qtwebengine update-chromium target, not in ALL, never built by a package build
cleanup|Chromium third_party (libzip): the qtwebengine CMake build adds no third_party project; GN builds it
cmake_uninstall|Chromium third_party (zstd), not in the CMake build
EigenUninstall|Chromium third_party (eigen3), not in the CMake build
GenerateMappings|Chromium third_party (dawn glfw), not in the CMake build
replace-enum-strings|Chromium third_party (harfbuzz-ng), not in the CMake build
runtest|Chromium third_party (eigen3 lapack), not in the CMake build
RunTest|Chromium third_party (opus), not in the CMake build
update-html|Chromium third_party (libzip man), not in the CMake build
update-man|Chromium third_party (libzip man), not in the CMake build
version|Chromium third_party (libaom), not in the CMake build
dist|Chromium third_party (libaom), not in the CMake build
generate_aom_config_templates|Chromium third_party (libaom), not in the CMake build
generate_exports|Chromium third_party (libaom), not in the CMake build
pkg_config|Chromium third_party (libaom, spirv-tools), not in the CMake build
write_pkg_config|Chromium third_party (spirv-tools), not in the CMake build
'

# <module> <file> <key>, tab-separated, sorted. Every site the scan sees.
SITES='
qtbase	cmake/QtBuildStaticDocToolsScript.cmake	QtSynchronizeRepo
qtbase	cmake/QtFlagHandlingHelpers.cmake	QtGenerateVersionScript
qtbase	cmake/QtHeadersClean.cmake	QtModuleHeadersCheck
qtbase	cmake/QtJavaHelpers.cmake	${setup_script}->QtSetupAndroid
qtbase	cmake/QtPkgConfigHelpers.cmake	QtFinishPkgConfigFile
qtbase	cmake/QtPriHelpers.cmake	QtGenerateExtPri
qtbase	cmake/QtPriHelpers.cmake	QtGenerateLibPri
qtbase	cmake/QtPrlHelpers.cmake	QtFinishPrlFile
qtbase	cmake/QtPublicCMakeHelpers.cmake	QtCopyFileIfDifferent
qtbase	cmake/QtPublicSbomCommonGenerationHelpers.cmake	${assemble_sbom}->assemble_sbom${suffix}${multi_config_suffix}
qtbase	cmake/QtPublicSbomGenerationHelpers.cmake	${reuse_lint_script}
qtbase	cmake/QtPublicSbomOpsHelpers.cmake	${script_wrapper}->run_sbom_source_verify_build_time_${arg_REPO_PROJECT_NAME_LOWERCASE}
qtbase	cmake/QtPublicTestHelpers.cmake	${environment_extras}
qtbase	cmake/QtTestHelpers.cmake	${arg_OUTPUT_FILE}
qtbase	cmake/QtTestHelpers.cmake	${benchmark_wrapper_file}->${target}Wrapper$<CONFIG>
qtbase	configure.cmake	check_zstd
qtbase	src/corelib/CMakeLists.txt	QtCompressMimeDatabase
qtbase	src/corelib/Qt6AndroidGradleHelpers.cmake	${qml_scan_script}->QtAndroidScanQmlImports
qtbase	src/corelib/Qt6AndroidGradleHelpers.cmake	${sign_package_script}->QtAndroidSignPackage
qtbase	src/corelib/Qt6CTestMacros.cmake	${build_tool_wrapper_file}->run_build_${testname}
qtbase	src/corelib/Qt6CTestMacros.cmake	${qmake_wrapper_file}->run_qmake_${testname}
qtbase	src/corelib/Qt6CTestMacros.cmake	${wrapper_file}->build_and_test_${testname},build_and_test_module_includes
qtbase	src/corelib/Qt6HarmonyOSMacros.cmake	${install_script}->HarmonyOSInstallHapFixture
qtbase	src/testinternal/3rdparty/cmake/QtRunCMakeTestHelpers.cmake	${QT_RUN_CMAKE_SCRIPT_PATH}->RunCMakeTest
qtbase	src/testinternal/3rdparty/cmake/RunCMake.cmake	${test}
qtbase	tests/auto/cmake/CMakeLists.txt	qt-cmake-private-install
qtbase	tests/auto/cmake/RunCMake/QtPrlHelpers/qt_finish_prl_file.cmake	QtFinishPrlFile
qtbase	tests/auto/cmake/test_android_signing/CMakeLists.txt	verify_signature
qtbase	tests/auto/cmake/test_clang_module_maps_disabled/CMakeLists.txt	verify_no_qt_modules
qtbase	tests/auto/cmake/test_moc_macro_target/CMakeLists.txt	check_moc_parameters
qtbase	tests/auto/cmake/test_qt_manual_moc/CMakeLists.txt	VerifyDefines
qtdeclarative	src/qml/Qt6QmlMacros.cmake	${copy_files_script_path}->Qt6QmlCopyFiles
qtdeclarative	src/qml/Qt6QmlMacros.cmake	Qt6UpdateQmllsIni
qtdeclarative	src/qml/Qt6QmlMacros.cmake	Qt6WriteQtConfPart
qtdeclarative	tests/auto/cmake/RunCMake/aot_lookup_validation/RunCMakeTest.cmake	ensure_aot_compiled
qtdeclarative	tests/auto/cmake/RunCMake/aot_lookup_validation/RunCMakeTest.cmake	verify_execution
qtwebengine	cmake/Functions.cmake	${symlinker_script}->QtWebEngineRspSymlinker
qtwebengine	cmake/Functions.cmake	QtGnCredits
qtwebengine	cmake/Functions.cmake	QtGnGen
qtwebengine	cmake/QtWebEngineSbomHelpers.cmake	QtGnSbom
qtwebengine	src/3rdparty/chromium/third_party/compiler-rt/src/cmake/Modules/CompilerRTUtils.cmake	cmake_install
qtwebengine	src/3rdparty/chromium/third_party/compiler-rt/src/include/CMakeLists.txt	cmake_install
qtwebengine	src/3rdparty/chromium/third_party/dawn/third_party/glfw/CMakeLists.txt	cmake_uninstall
qtwebengine	src/3rdparty/chromium/third_party/dawn/third_party/glfw/src/CMakeLists.txt	GenerateMappings
qtwebengine	src/3rdparty/chromium/third_party/eigen3/src/CMakeLists.txt	EigenUninstall
qtwebengine	src/3rdparty/chromium/third_party/eigen3/src/lapack/CMakeLists.txt	runtest
qtwebengine	src/3rdparty/chromium/third_party/harfbuzz-ng/src/CMakeLists.txt	replace-enum-strings
qtwebengine	src/3rdparty/chromium/third_party/libaom/source/libaom/build/cmake/aom_configure.cmake	generate_aom_config_templates
qtwebengine	src/3rdparty/chromium/third_party/libaom/source/libaom/build/cmake/aom_configure.cmake	version
qtwebengine	src/3rdparty/chromium/third_party/libaom/source/libaom/build/cmake/aom_install.cmake	pkg_config
qtwebengine	src/3rdparty/chromium/third_party/libaom/source/libaom/build/cmake/exports.cmake	generate_exports
qtwebengine	src/3rdparty/chromium/third_party/libaom/source/libaom/CMakeLists.txt	dist
qtwebengine	src/3rdparty/chromium/third_party/libaom/source/libaom/CMakeLists.txt	version
qtwebengine	src/3rdparty/chromium/third_party/libc++abi/src/include/CMakeLists.txt	cmake_install
qtwebengine	src/3rdparty/chromium/third_party/libc++abi/src/src/CMakeLists.txt	cmake_install
qtwebengine	src/3rdparty/chromium/third_party/libc++/src/include/CMakeLists.txt	cmake_install
qtwebengine	src/3rdparty/chromium/third_party/libc++/src/modules/CMakeLists.txt	cmake_install
qtwebengine	src/3rdparty/chromium/third_party/libc++/src/src/CMakeLists.txt	cmake_install
qtwebengine	src/3rdparty/chromium/third_party/libunwind/src/include/CMakeLists.txt	cmake_install
qtwebengine	src/3rdparty/chromium/third_party/libunwind/src/src/CMakeLists.txt	cmake_install
qtwebengine	src/3rdparty/chromium/third_party/libzip/src/man/CMakeLists.txt	update-html
qtwebengine	src/3rdparty/chromium/third_party/libzip/src/man/CMakeLists.txt	update-man
qtwebengine	src/3rdparty/chromium/third_party/libzip/src/regress/CMakeLists.txt	cleanup
qtwebengine	src/3rdparty/chromium/third_party/opus/src/CMakeLists.txt	RunTest
qtwebengine	src/3rdparty/chromium/third_party/spirv-tools/src/CMakeLists.txt	write_pkg_config
qtwebengine	src/3rdparty/chromium/third_party/zstd/src/build/cmake/lib/CMakeLists.txt	cmake_uninstall
qtwebengine	src/3rdparty/chromium/tools/clang/CMakeLists.txt	cmake_install
qtwebengine	src/core/CMakeLists.txt	SubmoduleUpdate
'

cat > "$SCRATCH/scan.py" <<'PY'
import os, re, sys
PAT = re.compile(r'(?<![\w-])-P"?\s+(?:\\?")?([^\s)"\\]+)')
SET = re.compile(r'set\(\s*(\w+)\s+"?([^\s)"]+)')
WANT = lambda f: f.endswith((".cmake", ".cmake.in")) or f == "CMakeLists.txt"

def script(tok):
    return tok.rsplit("/", 1)[-1][: -len(".cmake")] if tok.endswith(".cmake") else None

def key(tok, sets):
    v = re.fullmatch(r"\$\{(\w+)\}", tok)
    if v:
        tgt = sets.get(v.group(1))
        return "${%s}" % v.group(1) + ("->" + ",".join(sorted(tgt)) if tgt else "")
    return script(tok) or "?" + tok

def sites_in(text):
    text = "\n".join(l for l in text.splitlines() if not l.lstrip().startswith("#"))
    sets = {}
    for m in SET.finditer(text):
        s = script(m.group(2))
        if s:
            sets.setdefault(m.group(1), set()).add(s)
    return {key(m.group(1), sets) for m in PAT.finditer(text)}

def fail(msg):
    print(msg, file=sys.stderr)
    sys.exit(2)

mode, module, path = sys.argv[1:4]
out, nfiles = set(), 0
if mode == "tree":
    for d, _, files in os.walk(path):
        for f in files:
            if not WANT(f):
                continue
            nfiles += 1
            p = os.path.join(d, f)
            try:
                text = open(p, errors="replace").read()
            except OSError as e:
                fail(f"unreadable {p}: {e}")
            for k in sites_in(text):
                out.add((os.path.relpath(p, path), k))
else:  # patch: the lines it adds, per target file
    name = re.sub(r"^[a-z0-9]{32}-", "", os.path.basename(path))
    try:
        lines = open(path, errors="replace").read().splitlines()
    except OSError as e:
        fail(f"unreadable {path}: {e}")
    nfiles, cur = 1, None
    for l in lines:
        if l.startswith("+++ "):
            cur = re.sub(r"^[ab]/", "", l[4:].split("\t")[0].strip())
        elif l.startswith("+") and cur and WANT(os.path.basename(cur)):
            for k in sites_in(l[1:]):
                out.add((f"patch:{name}/{cur}", k))
print(f"FILES {nfiles}")
for rel, k in sorted(out):
    print(f"{module}\t{rel}\t{k}")
PY

# scan tree|patch <module> <path>: `FILES <n>` then site lines; exit 2 on an
# unreadable file.
scan() { python3 -I "$SCRATCH/scan.py" "$@"; }

KEYS=$(printf '%s\n' "$REVIEWED" | cut -d'|' -f1)
# Keys on stdin not reviewed: equality with a list entry, never a substring.
unreviewed() {
  while IFS= read -r k; do
    [ -n "$k" ] || continue
    printf '%s\n' "$KEYS" | command grep -qxF -- "$k" || printf '%s\n' "$k"
  done
}
# Site lines on stdin not in <list>.
not_in() {
  while IFS= read -r s; do
    [ -n "$s" ] || continue
    printf '%s\n' "$1" | command grep -qxF -- "$s" || printf '%s\n' "$s"
  done
}

# --- self-control -----------------------------------------------------------
sc="$SCRATCH/selfcontrol"; mkdir -p "$sc/t/sub" "$sc/empty"
cat > "$sc/t/CMakeLists.txt" <<'CM'
add_custom_command(OUTPUT b COMMAND ${CMAKE_COMMAND} -P ${D}/PlantedLiteral.cmake)
add_custom_command(OUTPUT c COMMAND ${CMAKE_COMMAND} -P "${planted_variable}")
add_custom_command(OUTPUT d COMMAND ${CMAKE_COMMAND} -P
    "${D}/PlantedNextLine.cmake")
set(text "cmake -P \"${D}/PlantedEscaped.cmake\"")
add_test(NAME e COMMAND "${CMAKE_COMMAND}" "-P" "${D}/PlantedQuotedFlag.cmake")
set(copy_files_script_path "${D}/PlantedRepointed.cmake")
add_custom_command(OUTPUT f COMMAND ${CMAKE_COMMAND} -P "${copy_files_script_path}")
# a comment saying -P ${D}/PlantedComment.cmake is not an edge
CM
printf 'add_custom_command(OUTPUT a COMMAND ${CMAKE_COMMAND} -P "${D}/QtFinishPrlFile.cmake")\n' \
  > "$sc/t/sub/CMakeLists.txt"
cat > "$sc/planted.patch" <<'PATCH'
--- a/cmake/X.cmake
+++ b/cmake/X.cmake
@@ -1,1 +1,2 @@
 set(a b)
+add_custom_command(OUTPUT p COMMAND ${CMAKE_COMMAND} -P ${D}/PlantedByPatch.cmake)
PATCH
sc_sites=$( { scan tree selftest "$sc/t"; scan patch selftest "$sc/planted.patch"; } | command grep -v '^FILES ')
want_keys='${copy_files_script_path}->PlantedRepointed,${planted_variable},PlantedByPatch,PlantedEscaped,PlantedLiteral,PlantedNextLine,PlantedQuotedFlag'
got_keys=$(printf '%s\n' "$sc_sites" | cut -f3 | unreviewed | sort | paste -sd, -)
if [ "$got_keys" != "$want_keys" ]; then
  echo "NO ANSWER: self-control: unreviewed keys read as '$got_keys', not '$want_keys'"
  exit 2
fi
# QtFinishPrlFile is reviewed, so only the SITE check can name it at a new file.
known=$(printf 'selftest\tCMakeLists.txt\tPlantedLiteral\n')
if ! printf '%s\n' "$sc_sites" | not_in "$known" | command grep -qxF "$(printf 'selftest\tsub/CMakeLists.txt\tQtFinishPrlFile')"; then
  echo "NO ANSWER: self-control: a reviewed script at an unlisted site was not named"
  exit 2
fi
if printf '%s\n' "$sc_sites" | command grep -q PlantedComment; then
  echo "NO ANSWER: self-control: a comment was read as a site"
  exit 2
fi
if [ "$(scan tree selftest "$sc/empty")" != "FILES 0" ]; then
  echo "NO ANSWER: self-control: an empty tree yielded files or sites"
  exit 2
fi
echo "self-control OK: each spelling, a repointed variable and a patch-added -P are unreviewed keys; a reviewed script at a new file is a new site; a comment and an empty tree yield nothing"

# --- the consumer's Qt modules ----------------------------------------------
TARGET=${1:-${NN_QT_TARGET:-}}
if [ -z "$TARGET" ] || [ ! -e "$TARGET" ]; then
  echo "NO ANSWER: no target derivation (argument or NN_QT_TARGET): '${TARGET}'"
  exit 2
fi
X=(--extra-experimental-features 'nix-command dynamic-derivations ca-derivations recursive-nix')
closure=$(nix path-info "${X[@]}" -r "$TARGET" 2>/dev/null); rc=$?
if [ $rc -ne 0 ] || [ -z "$closure" ]; then
  echo "NO ANSWER: closure of $TARGET unreadable (rc=$rc)"
  exit 2
fi
# Every `qt*` derivation is a module, its source tarball or a setup hook; any
# other is a shape this gate does not know, so it cannot claim completeness.
qts=$(printf '%s\n' "$closure" | sed -E 's|^/nix/store/[a-z0-9]{32}-||' | command grep -E '^qt.*\.drv$')
odd=$(printf '%s\n' "$qts" | command grep -vE '^qt[a-z0-9]+-[0-9][0-9.]*\.drv$|-everywhere-src-[0-9.]+\.tar\.xz\.drv$|-setup-hook\.drv$' | command grep .)
if [ -n "$odd" ]; then
  echo "NO ANSWER: qt derivation(s) neither a module, a source nor a setup hook: $(printf '%s' "$odd" | paste -sd' ' -)"
  exit 2
fi
mods=$(printf '%s\n' "$closure" | command grep -E -- '/[a-z0-9]{32}-qt[a-z0-9]+-[0-9][0-9.]*\.drv$')
nmods=$(printf '%s\n' "$mods" | command grep -c . )
if [ "$nmods" -eq 0 ]; then
  echo "NO ANSWER: no qt module derivation in the closure of $TARGET"
  exit 2
fi

: > "$SCRATCH/sites"; seen=
for drv in $mods; do
  name=$(basename "$drv" .drv | sed -E 's/^[a-z0-9]{32}-//')
  module=${name%-*}; seen="$seen $module"
  if ! json=$(nix derivation show "${X[@]}" "$drv" 2>/dev/null); then
    echo "NO ANSWER: $name: nix derivation show failed for $drv"
    exit 2
  fi
  attrs=$(printf '%s' "$json" | python3 -I -c '
import json, sys
v = next(iter(json.load(sys.stdin)["derivations"].values()))
s = v.get("structuredAttrs") or v["env"]
p = s.get("patches", "")
print(s.get("src", ""))
print(" ".join(p) if isinstance(p, list) else p)')
  src=$(printf '%s\n' "$attrs" | sed -n 1p)
  patches=$(printf '%s\n' "$attrs" | sed -n 2p)
  if [ -z "$src" ] || [ ! -e "$src" ]; then
    echo "NO ANSWER: $name: source '$src' is not resident"
    exit 2
  fi
  if [ -d "$src" ]; then
    tree=$src
  else
    x="$SCRATCH/$name"; mkdir -p "$x"
    # Listed then extracted by name: tar exits non-zero when any one
    # wildcard matches nothing, and a module with no `*.cmake.in` is fine.
    if ! tar tf "$src" > "$x.members" 2>/dev/null; then
      echo "NO ANSWER: $name: $src could not be listed"
      exit 2
    fi
    command grep -E '(\.cmake|\.cmake\.in|/CMakeLists\.txt)$' "$x.members" > "$x.wanted"
    if [ -s "$x.wanted" ] && ! tar xf "$src" -C "$x" -T "$x.wanted" 2>/dev/null; then
      echo "NO ANSWER: $name: cmake files of $src could not be extracted"
      exit 2
    fi
    # The tarball's one top directory carries the version; sites are below it.
    tops=$(ls -A "$x")
    if [ "$(printf '%s\n' "$tops" | command grep -c .)" -ne 1 ]; then
      echo "NO ANSWER: $name: $src does not unpack to one top directory"
      exit 2
    fi
    tree="$x/$tops"
  fi
  if ! out=$(scan tree "$module" "$tree"); then
    echo "NO ANSWER: $name: a cmake file under $src could not be read"
    exit 2
  fi
  nfiles=$(printf '%s\n' "$out" | head -1 | sed 's/^FILES //')
  if [ "$nfiles" = 0 ]; then
    echo "NO ANSWER: $name: no cmake files read from $src"
    exit 2
  fi
  printf '%s\n' "$out" | tail -n +2 | command grep . >> "$SCRATCH/sites"
  np=0
  for p in $patches; do
    if ! pout=$(scan patch "$module" "$p"); then
      echo "NO ANSWER: $name: patch $p could not be read"
      exit 2
    fi
    printf '%s\n' "$pout" | tail -n +2 | command grep . >> "$SCRATCH/sites"
    np=$((np + 1))
  done
  n=$(printf '%s\n' "$out" | tail -n +2 | command grep -c .)
  echo "$name: $nfiles cmake files, $np patches, $n -P sites in the source"
done

fail=0
sort -u "$SCRATCH/sites" -o "$SCRATCH/sites"
listed=$(printf '%s\n' "$SITES" | command grep .)
new=$(not_in "$listed" < "$SCRATCH/sites")
# A listed site of a module this target does not reach is not judged here.
gone=$(printf '%s\n' "$listed" | while IFS= read -r s; do
  case " $seen " in *" ${s%%	*} "*) printf '%s\n' "$s" ;; esac
done | not_in "$(cat "$SCRATCH/sites")")
keys=$(cut -f3 "$SCRATCH/sites" | sort -u | unreviewed)
while IFS= read -r s; do [ -n "$s" ] && { echo "  NEW SITE: $s"; fail=1; }; done <<< "$new"
while IFS= read -r s; do [ -n "$s" ] && { echo "  GONE SITE: $s"; fail=1; }; done <<< "$gone"
while IFS= read -r k; do [ -n "$k" ] && { echo "  UNREVIEWED: $k"; fail=1; }; done <<< "$keys"

if [ $fail -ne 0 ]; then
  echo "FAIL: the Qt modules' cmake -P sites differ from the reviewed list; read each named script's source for undeclared writes, then update SITES and REVIEWED"
  exit 1
fi
nsites=$(command grep -c . "$SCRATCH/sites")
nkeys=$(cut -f3 "$SCRATCH/sites" | sort -u | command grep -c .)
echo "PASS: every cmake -P site the consumer's $nmods Qt modules carry is listed and every script reviewed ($nsites sites, $nkeys scripts)"
