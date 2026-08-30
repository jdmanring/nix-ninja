project('example-generated-header', 'cpp',
  version : '0.1.0')

# THE POINT OF THIS EXAMPLE: a header that does not exist when the driver
# scans, produced by a custom target during the build.
#
# meson declares a generated header ORDER-ONLY (`||`) on every compile edge
# in the target, so `#include "version.gen.h"` resolves through `-I` to a
# path nothing has written yet. That is the shape libvmaf's `vcs_version.h`
# has, the shape cmake's FortranCInterface `VerifyFortran.h` has, and the
# shape that failed three separate ways when nix itself was first driven
# through this tool.
#
# A generated SOURCE is a different and easier case, already covered by
# example-dynamic-deps: the driver never resolves an include against a file
# that is not there yet.
#
# THE HEADER IS IN A SUBDIRECTORY ON PURPOSE, and the first version of this
# example got it wrong. With the output at the build root and `-I.`, the
# scan resolves the include as `./version.gen.h` while the graph knows it as
# `version.gen.h`; those differ as paths, the virtual lookup misses, and the
# include is never resolved at all - so the case under test never runs and
# the example passed with the fix REMOVED. In a subdirectory the include dir
# is `gen` and the spelling joins to exactly the path the graph declares,
# which is what nix's `src/nix/nix.p/...gen.hh` does.
subdir('gen')

executable('main',
  ['main.cpp', version_h],
  include_directories : include_directories('gen'))
