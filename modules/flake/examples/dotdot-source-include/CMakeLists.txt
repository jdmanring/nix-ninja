# A GENERATED SOURCE INCLUDING ANOTHER SOURCE BY AN ABSOLUTE DOTDOT PATH,
# which is openblas's kernel layout: cmake writes a small .c into the build
# directory whose only content is an include of a real implementation, spelled
# through a sibling directory:
#
#   #include "/build/source/kernel/x86_64/../generic/imatcopy_ct.c"
#
# Two things have to be true in the sandbox for that to open, and the second
# is easy to miss: the file must be materialized at its CANONICAL path, and
# the intermediate directory named before the `..` must EXIST, because the
# kernel resolves `..` against a real directory rather than lexically.
#
# kernel/x86_64 deliberately holds nothing the build needs, so nothing else
# would cause it to be created.
cmake_minimum_required(VERSION 3.20)
project(dotdot C)
configure_file(gen.c.in gen.c @ONLY)
add_executable(app ${CMAKE_CURRENT_BINARY_DIR}/gen.c main.c)
