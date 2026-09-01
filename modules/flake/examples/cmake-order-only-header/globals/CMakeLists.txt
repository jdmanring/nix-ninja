# THREE translation units and only TWO include the generated header, which
# is svt-av1's GLOBALS target exactly. The third is the control: it shares
# every order-only edge with the other two and must keep building whatever
# the scan does with the header.
add_library(GLOBALS OBJECT version.c settings.c metadata.c)
add_dependencies(GLOBALS VersionGen)
target_include_directories(GLOBALS PRIVATE ${CMAKE_BINARY_DIR}/lib)
