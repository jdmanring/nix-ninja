# FortranCInterface_VERIFY() at CONFIGURE time, which is the shape liblapack
# dies on. Detection compiles a Fortran object, reads the mangling out of the
# BUILT ARTIFACT, and writes a header carrying the macro; the verify project
# then compiles C against it. Every step runs through the driver during
# configure, before any target of this project is built.
#
# Its failure is SILENT at the compile that is wrong: a header whose symbol
# section came out empty still compiles, and the error surfaces a step later
# as an undefined reference to the unmangled name.
cmake_minimum_required(VERSION 3.20)
project(fortranc C Fortran)
include(FortranCInterface)
FortranCInterface_VERIFY()
add_executable(app main.c)
