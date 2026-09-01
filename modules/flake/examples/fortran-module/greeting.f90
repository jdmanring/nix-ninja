module greeting
  implicit none
contains
  subroutine say_hello()
    print *, 'hello from a fortran module'
  end subroutine say_hello
end module greeting
