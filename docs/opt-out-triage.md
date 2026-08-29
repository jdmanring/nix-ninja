# The ArtNix opt-out list: thirteen packages, six families

ArtNix keeps a list of packages nix-ninja does not drive (`nnOptOut` in
`site/base/default.nix`). Every entry costs that package's per-TU
resumability, which is the whole reason this tool exists, so the list is a
defect record and not a configuration choice. It is worked down, never
extended for convenience.

Opened 2026-08-29. Four entries were retired that day (bison,
hicolor-icon-theme, libvmaf, liblapack) and are not repeated here; the
thirteen below are what remains.

## The split that decides what can be worked today

Eight have a RECORDED CAUSE and can be worked from the code. Five do not: the
`--keep-going` round of 2026-08-26 that produced the list recorded them as
"each with its own signature in that round's log", and that log has not
survived. They are recoverable only by another `--keep-going` pass.

**Do not guess at the five.** A fix aimed at an unseen failure is a change
whose only evidence is that it compiled.

## Family 1 - undeclared SOURCE inputs to a custom command

`onetbb` (reads `integration/linux/env/vars.sh.in`), `svt-av1` (reads
`ConfigureGitVersion.cmake`).

A custom command reads a file that lives in the SOURCE tree and that no ninja
edge declares. The build-dir blanket cannot carry these because it walks the
build directory, and these are not in it.

Nearest existing machinery: the `virtual_paths` map added in `2f7b8f2`, which
solved the same shape for GENERATED headers by mapping build paths from
`task.inputs`. The source-tree case needs the equivalent for paths that are
neither declared inputs nor build-dir residents.

## Family 2 - cmake_install.cmake file operations

`c-ares`, `wildmidi`.

## Family 3 - a directory passed where a file is expected

`webrtc-audio-processing`, failing `Is a directory (os error 21)`.

The error names a specific unhandled case rather than a design gap, which
makes this the most likely of the eight to be small. It was.

**Diagnosed 2026-08-29 from the per-derivation nix log, which had survived
even though the `--keep-going` round log had not**
(`/nix/var/log/nix/drvs/rd/gsz7fi...-webrtc-audio-processing-2.1.drv.bz2`,
and three more across 1.3 and 2.1). Every compile edge in the graph, from
`BuildId(1)` onward:

    Error: Failed to build task derivation for task
    Caused by:
        Failed to read file /build/source/webrtc/rtc_base/memory: Is a directory (os error 21)

1.3 adds a second spelling, `modules/audio_processing/utility`.

`rtc_base/memory` is a DIRECTORY holding `aligned_malloc.h`, and
`rtc_base/platform_thread.cc` and `rtc_base/buffer.h` write
`#include <memory>`. `canonicalize_cached` accepted it because
`Path::exists()` is true for a directory, `try_resolve` returned it as the
resolved include, `bfs_parse_includes` queued it, and `scan_directives`'
`fs::read` died EISDIR. gcc skips a directory on the include path and keeps
searching; the scanner stopped there.

Fixed at the single resolution point in
`crates/deps-infer/src/c_include_parser.rs`: resolution now refuses a
directory, so the search falls through to the next include dir as the
compiler's does. Regression test in
`crates/nix-ninja/tests/include_dir_shadow.rs` - deliberately outside
`nix-ninja-task`'s src allowlist - which fails on the old `exists()` check.

**Not retired.** The fix is in `crates/deps-infer`, inside that allowlist, so
it re-keys the task binary and every banked per-TU output; and the package
has not been rebuilt. Batch it with the other pending re-keying changes and
take the entry out of `nnOptOut` when webrtc-audio-processing builds.

## Family 4 - meson depfile handling

`dav1d`, missing `.obj.ndep`.

## Family 5 - the implicit-input blanket past its limit

`openblas`, at 8,299 build-dir inputs against `IMPLICIT_INPUTS_LIMIT = 512`.

**Raising the limit is the wrong fix and the comment at that constant says
why**: at Chromium scale the blanket is a memory bomb, measured at seven
daemon workers holding ~2 GiB each. The limit is doing its job; openblas
loses because the blanket is a blunt instrument, not because the bound is
wrong.

The direction is named in the code, a few lines above the constant: parse the
includes and add them to the search path, so dependencies are discovered
precisely rather than injected wholesale. That retires the blanket for the
compile case instead of tuning it.

## Family 6 - a directory-shaped unit

`corrosion`. Its cmake tests drive `cargo rustc`, and cargo works on a crate
DIRECTORY rather than the file set a ninja edge declares. Every failure read
`can't find library ..., rename file to src/lib.rs`.

This is a shape mismatch rather than a scanner gap, and it may be genuinely
out of scope. If it is, that conclusion belongs here in writing, with what
was tried - "out of scope" and "nobody has tried" produce identical evidence.

## Signature unrecovered

`openh264`, `p11-kit`, `libssh`, `valgrind`, `x265`.

Recover with a `--keep-going` pass over `.#artnix-server` and record each
signature HERE as it appears, before proposing any fix for it.

## Working rules for this list

- One family at a time, with a test that fails before the fix.
- Verify against the actual package, not only a unit test. A family is
  retired when its packages build, and the entry comes out of `nnOptOut` in
  the same change.
- A package that fails again comes back with its FRESH signature recorded
  here. That is a better record than a name in a list.
- Mind the cost class before editing: `crates/nix-ninja` is outside
  `nix-ninja-task`'s src allowlist, but an edit to what the driver EMITS
  re-keys every banked output anyway. `CLAUDE.md` has both tests.
