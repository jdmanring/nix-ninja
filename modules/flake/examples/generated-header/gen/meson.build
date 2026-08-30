# The header lands in THIS subdirectory, so the compile edge sees
# `-Igen` and the include spelling joins to exactly `gen/version.gen.h`.
# That detail is the whole point: see the parent meson.build.
version_h = custom_target('version_h',
  output : 'version.gen.h',
  input : 'genhdr.sh',
  command : [find_program('bash'), '@INPUT@', '@OUTPUT@'],
)
