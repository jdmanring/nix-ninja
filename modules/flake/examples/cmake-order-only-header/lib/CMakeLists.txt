# ONE EDGE WITH TWO OUTPUTS, which is what svt-av1 has and what a plain
# add_custom_command(OUTPUT) does not produce. `add_custom_target` with
# BYPRODUCTS makes CMake emit a single CUSTOM_COMMAND whose outputs are the
# target's stamp AND the generated header:
#
#   build lib/CMakeFiles/VersionGen lib/gen_version.h: CUSTOM_COMMAND
#
# The header is therefore a CO-OUTPUT of a stamp edge rather than the sole
# output of its own, and it is reached from a compile edge through two
# order-only phony hops.
add_custom_target(VersionGen
  COMMAND ${CMAKE_COMMAND} -E echo "#define LIB_VERSION \"generated-4.1.0\"" > ${CMAKE_CURRENT_BINARY_DIR}/gen_version.h
  BYPRODUCTS ${CMAKE_CURRENT_BINARY_DIR}/gen_version.h
  VERBATIM)
