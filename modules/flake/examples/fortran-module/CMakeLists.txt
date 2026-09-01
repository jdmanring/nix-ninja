# ORDINARY FORTRAN, no configure-time detection: one module defined in one
# file and used in another, which is the smallest build CMake emits a ninja
# dyndep binding for. If this fails on the DRV route then the defect is every
# Fortran package rather than every package verifying Fortran/C interop.
cmake_minimum_required(VERSION 3.20)
project(fortranmod Fortran)
add_executable(app greeting.f90 main.f90)
