#!/usr/bin/env bash
# WHY compiler-rt's .syms BYPRODUCTS DECLARATION DOES NOT COMPILE.
#
# This gate asserts about CMAKE and not about this tree, so it is in neither
# runner. It exists because the reason was guessed twice, in two sessions,
# from the SHAPE OF THE ERROR, and both guesses named ordering: "the macro
# runs before its runtime target is created". That is false. The target is
# created first here by construction, one line above, and the failure is
# identical.
#
# Four arms, and the pair that discriminates is the last two:
#
#   1 BYPRODUCTS $<CONFIG>            builds  generator expressions are
#                                             allowed in the field at all
#   2 COMMAND    $<TARGET_FILE:foo>   builds  the expression resolves at
#                                             that site, in that call
#   3 BYPRODUCTS $<TARGET_FILE:foo>   FAILS   No target "foo"
#   4 BYPRODUCTS composed plain path  builds  the remedy
#   5 composed path == TARGET_FILE     equal   the remedy is the SAME file
#
# Arm 5 is the one that keeps arm 4 honest. A byproduct naming a path the
# command does not write declares a file nothing produces, which fails at the
# same install line for a new reason, so "a plain path is accepted" is only
# half of it. It sets the two properties compiler-rt's own
# `set_target_output_directories` and `OUTPUT_NAME` set, then compares a
# `file(GENERATE)` of `$<TARGET_FILE>` against the composition, rather than
# arguing that the pieces line up.
#
# So the restriction is on TARGET-dependent expressions in OUTPUT and
# BYPRODUCTS, which are read while the directory is configured and no
# generator targets exist, while COMMAND and install(FILES) defer to
# generate time. The `.syms` suffix is not a term: arm 3 fails bare.
#
# WHAT IT MEANS FOR THE PACKAGE: a declaration IS available at that site,
# spelled without a target expression. compiler-rt has the pieces in scope,
# `get_compiler_rt_output_dir` and `set_output_name`, both already used by
# `add_compiler_rt_runtime`. The composed path has to equal TARGET_FILE
# exactly, prefix and suffix included, and `add_sanitizer_rt_symbols` is
# called for static and shared runtimes alike, so composing it is where a
# patch can still be wrong.
#
#   PASS   all four arms answer as above
#   FAIL   any arm disagrees
#   CMAKE=<path>   the cmake to ask (default: the one on PATH)
set -u
CM=${CMAKE:-cmake}
command -v "$CM" >/dev/null || { echo "NO ANSWER: no cmake"; exit 2; }
echo "cmake: $("$CM" --version | head -1)"
D=$(mktemp -d -t nn-byp-XXXXXX)
trap 'rm -rf "$D"' EXIT
printf 'int f(void){return 0;}\n' > "$D/a.c"
HDR='cmake_minimum_required(VERSION 3.20)
project(byp C)
add_library(foo STATIC a.c)'
fail=0
arm() {  # arm <name> <expected: build|error> <body>
  printf '%s\n%s\n' "$HDR" "$3" > "$D/CMakeLists.txt"
  rm -rf "$D/b"
  out=$("$CM" -G Ninja -S "$D" -B "$D/b" 2>&1); rc=$?
  if [ $rc -eq 0 ]; then got=build; else got=error; fi
  if [ "$got" = "$2" ]; then verdict=ok; else verdict=UNEXPECTED; fi
  [ "$verdict" = ok ] || fail=1
  note=
  [ "$got" = error ] && note=$(printf '%s' "$out" | grep -m1 'No target' | sed 's/^ *//')
  printf '  %-28s expected %-5s got %-5s %s %s\n' "$1" "$2" "$got" "$verdict" "$note"
}
STAMP='${CMAKE_CURRENT_BINARY_DIR}/stamp'
TAIL="add_custom_target(t ALL DEPENDS $STAMP)"
arm 'BYPRODUCTS $<CONFIG>' build "add_custom_command(OUTPUT $STAMP
  BYPRODUCTS \${CMAKE_CURRENT_BINARY_DIR}/\$<CONFIG>.syms
  COMMAND \${CMAKE_COMMAND} -E touch $STAMP VERBATIM)
$TAIL"
arm 'COMMAND $<TARGET_FILE>' build "add_custom_command(OUTPUT $STAMP
  COMMAND \${CMAKE_COMMAND} -E touch \$<TARGET_FILE:foo>.syms
  COMMAND \${CMAKE_COMMAND} -E touch $STAMP
  DEPENDS foo VERBATIM)
$TAIL"
arm 'BYPRODUCTS $<TARGET_FILE>' error "add_custom_command(OUTPUT $STAMP
  BYPRODUCTS \$<TARGET_FILE:foo>.syms
  COMMAND \${CMAKE_COMMAND} -E touch $STAMP VERBATIM)
$TAIL"
arm 'BYPRODUCTS plain path' build "add_custom_command(OUTPUT $STAMP
  BYPRODUCTS \${CMAKE_CURRENT_BINARY_DIR}/libfoo.a.syms
  COMMAND \${CMAKE_COMMAND} -E touch \${CMAKE_CURRENT_BINARY_DIR}/libfoo.a.syms
  COMMAND \${CMAKE_COMMAND} -E touch $STAMP VERBATIM)
$TAIL"
# ARM 5: the composition has to be the same file the command writes.
cat > "$D/CMakeLists.txt" <<'EOF'
cmake_minimum_required(VERSION 3.20)
project(byp C)
add_library(foo STATIC a.c)
set_target_properties(foo PROPERTIES
  OUTPUT_NAME clang_rt.ubsan_standalone-x86_64
  ARCHIVE_OUTPUT_DIRECTORY ${CMAKE_CURRENT_BINARY_DIR}/lib/linux)
set(composed "${CMAKE_CURRENT_BINARY_DIR}/lib/linux/${CMAKE_STATIC_LIBRARY_PREFIX}clang_rt.ubsan_standalone-x86_64${CMAKE_STATIC_LIBRARY_SUFFIX}")
file(GENERATE OUTPUT ${CMAKE_CURRENT_BINARY_DIR}/target_file.txt CONTENT "$<TARGET_FILE:foo>")
file(WRITE ${CMAKE_CURRENT_BINARY_DIR}/composed.txt "${composed}")
EOF
rm -rf "$D/b"
if "$CM" -G Ninja -S "$D" -B "$D/b" >/dev/null 2>&1 &&
   [ "$(cat "$D/b/target_file.txt")" = "$(cat "$D/b/composed.txt")" ]; then
  printf '  %-28s expected equal got equal ok\n' 'composed == TARGET_FILE'
else
  fail=1
  printf '  %-28s expected equal got DIFFERENT UNEXPECTED\n' 'composed == TARGET_FILE'
  printf '    target file %s\n    composed    %s\n' \
    "$(cat "$D/b/target_file.txt" 2>/dev/null)" "$(cat "$D/b/composed.txt" 2>/dev/null)"
fi

if [ $fail -eq 0 ]; then
  echo "PASS: OUTPUT and BYPRODUCTS refuse a target expression; a composed plain path is accepted and names the same file"
  exit 0
fi
echo "FAIL: an arm disagreed, so the recorded reason does not hold on this cmake"
exit 1
