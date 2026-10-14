# Changelog

Every milestone the tree has reached, newest first, each pinned to the commits
that delivered it and to the gate that verified it. A row is added when a
deliverable is VERIFIED, not when work starts.

"Newest first" orders by VERSION, then by date within a version: the rows run
`0.3.0` before `0.2.0` before `0.1.0`, and each block newest date first. So a
`0.1.0` row can carry a later date than the `0.2.0` block above it, because the
version is the milestone the work satisfies and the date is when that work
landed. Reading the table as date-ordered is the one mistake this sentence
exists to prevent.

The date is the day the row's work was last delivered, which is the date of the
latest commit the row cites. It is not the day the work began and not the day
the row was written.

Versions are milestones, not calendar releases. The number says how far along
[the roadmap](docs/ROADMAP.md) the tree is; before 1.0 it says nothing about
stability.

`0.1` and `0.2` are `pdtpartners/nix-ninja`'s, defined as correctness and as
performance in that repository's README. `0.3` and after are this project's own.

A row's version is the milestone whose criterion its work advances, not the
date it landed: work on a `0.1` criterion finished in September is a `0.1.0`
row, and the roadmap's criterion text is what decides it. Work that advances no
named criterion yet sits under the milestone currently in progress, which is
`0.3.0`; that is a statement about where the work belongs, not that the `0.3`
criterion is met. The roadmap says the same, and names the classes still open.

Every row pins a commit whose subject still matches it. A row naming the wrong
commit is worse than no row, because it reads as checked.

A cell that says "a gate" names an instrument kept beside this project's
development rather than in this repository, which is why it describes what the
gate DOES rather than naming its file. The instruments that live in the tree are
the tests `nix flake check` runs. Publishing the rest is tracked in
[docs/ROADMAP.md](docs/ROADMAP.md).

| version | date | delivered | verified by |
|---|---|---|---|
| 0.3.0 | 2026-10-13 | The compiler refusal stops firing for a task that execs its toolchain by absolute path, and starts naming the command it refuses. mesa-libclc 22.1.8.3 failed 514 tasks with `` `cc` is not on PATH `` on commands that never invoke cc, because the name test strips the directory before matching and matched the absolute `.../bin/clang`; the refusal now skips when nothing on the line needs a PATH lookup, and a bare name still refuses. Without that line the two causes are indistinguishable afterwards, because a refused edge emits no derivation at all. (`240050a`, `4b3bd7d`) | The unit test asserts the old behaviour is wrong before the fix exists, and a negative control keeps a bare toolchain name refusing. An earlier and broader version, filtering every absolute token out of the name test, broke `a_compiler_by_any_spelling_is_named` and was discarded. Priced FREE both times, task drvPath unmoved and five emission shapes identical, with the three control arms firing before either verdict |
| 0.3.0 | 2026-10-13 | The macro-arm include reader reads the angle-bracket spelling. A macro defined once per arm as `<name.inc>` was dropped in silence, because the reader matched a double-quoted body only, so the header it names was declared nowhere and every OpenCL compile of mesa-libclc 22.1.8.3 died on a missing `.inc` under `#include __CLC_BODY`. (`8622e7a`) | The unit test fails against the unfixed reader with an empty result and passes after, and a second test fixes the resolution and the absent-arm behaviour so the first cannot be satisfied by accepting any bracketed token. Measured on the failing task's own declared inputs: two arms from two define sites, both angle-bracket, both on disk |
| 0.3.0 | 2026-10-12 | An ExternalProject sub-build gets its ninja and its binary dir. The nghttp2 sub-build runs `cmake -GNinja` and the emitted PATH carried no ninja, because `which` resolves through the outer PATH where ninja is not in the store; the configure step also WRITES `BINARY_DIR` and declared only its stamp. (`34038d2`, `e93c9da`, `425778a`) | A B gate runs a real cmake configure inside an ExternalProject: head declares the binary dir, puts ninja on PATH and carries it to the build step; the pin control fails at both walls with the error the real derivation died on. A second gate drives the flag-pair guard for the defect that shipped twice |
| 0.3.0 | 2026-10-11 | The ExternalProject chain is complete. Five walls, each found by the next step failing: a download step extracting into `SOURCE_DIR` as an undeclared side effect; an `extract-<name>.cmake` resolved against the step's leading `cd`; a patch step running INSIDE `SOURCE_DIR` so the task copies a directory it both receives and declares, writable; step edges made stamp edges with the tree riding the stamp as a co-output; and a later step's tree superseding the earlier registration. (`b0dc02a`, `0922958`, `362d801`, `e657c80`, `ba8ee4e`, `e1101af`) | A B-configuration gate drives a real ExternalProject, store-path archive, patch and build step scored on the bytes it saw; head sees the patched tree, the pin control does not. Mutation-checked in both directions: disabling the supersede gives the original tree, disabling the writable copy aborts the patch step |
| 0.3.0 | 2026-10-10 | `protoc` imports resolve through the include roots, and every qualifier is read. No edge declares a `.proto`'s import siblings, so 134 tasks failed across two desktop rounds. The reader resolves in protoc's own first-root-wins order and recurses the closure; a second commit reads every qualifier up to the opening quote, because `edition = "2024"`'s `import option` was skipped in silence and 162 more tasks failed. (`16a3e01`, `526d293`) | An A gate drives a protoc edge shaped like the failing derivation and asserts every import form reaches the emitted input list, with the pin as control. Population counted rather than assumed: `import "` 247, `import option "` 33, `import public "` 27 |
| 0.3.0 | 2026-10-09 | A `cmake -P` script no edge declares is carried, including the script itself. An ExternalProject configure writes `<name>-mkdirs.cmake`, and the reader read the script's contents for targets without ever declaring the script, so the task ran with an empty input list and died `Not a file`. A reader that reads a file must declare the file. (`fe4e173`) | An A gate, with a control on the commit before the fix that fails it |
| 0.3.0 | 2026-10-05 | The make-directory list rides `passAsFile`. `NIX_NINJA_MAKE_DIRS` accumulated one entry per directory and scaled with the package: one archive edge emitted 132,267 bytes against the kernel's fixed 131,072 limit and `execve` refused it over four short argv entries. Intermittent by construction, since it crosses the limit only on the largest subjects. (`731d908`) | An A gate asserts the list is in `passAsFile` exactly where a task carries it and nowhere when it does not, with a control that carries both lists inline. Priced both ways by measurement: appending unconditionally moved all five priced shapes, appending conditionally left them byte identical |
| 0.3.0 | 2026-10-03 | An edit that removes a source product replaces it, so the next run does not read a stale build tree. (`4ef51fd`, `34f75f5`) | An A gate that pins the behavior, plus the ninja-state gates |
| 0.3.0 | 2026-09-06 | A linker script another edge writes is carried, and a `-Wl` script is resolved from the graph before the disk. (`e4cb09b`, `21767b0`, `6a35958`) | An A gate, and a B gate on the same shape |
| 0.3.0 | 2026-09-06 | A generated object reaches the compiler when its edge is a generator, and an include directory named above the build directory is carried rather than dropped. (`3ec8201`, `132b372`) | A gate on the output-shape arm, and the empty and unstaged include-dir arms |
| 0.3.0 | 2026-09-05 | The file a soname alias points at is carried into a shared-library link; without it the staged library's `DT_NEEDED` is not an input. (`5235867`) | An A gate, red on the revision before the fix |
| 0.3.0 | 2026-09-05 | A header a package stages into its own output is carried, and edges that also WRITE there are handled. (`cc08008`, `435c1b1`, `48a72e4`) | A B gate that scores the bytes landing in the output, with a control that builds and fails the assertion |
| 0.3.0 | 2026-09-05 | The files a `.gresource.xml` manifest lists are carried. (`143415f`) | An A gate and a B gate |
| 0.3.0 | 2026-09-05 | A declared output the command never wrote is named as such rather than left to fail later. (`41de370`) | An A gate covering both halves |
| 0.3.0 | 2026-09-04 | Three routes by which a later driver pass adopts a placed product as an input are closed. (`f2dba47`) | An A gate running three passes, and a full B build |
| 0.3.0 | 2026-09-04 | An rst document's `.. include::` fragments are read, for a writer handed one. (`afdfa7c`) | A unit test, and a package driven on B |
| 0.3.0 | 2026-09-02 | The XML catalog is carried into a task. (`ee27295`) | An A gate |
| 0.3.0 | 2026-09-02 | A `cmake -P` script is read for the input paths it composes. (`7d6acc9`) | An A gate asserting the alias consumer's inputs |
| 0.3.0 | 2026-08-29 | `cc` is resolved lazily: a package with no compile targets does not need one. (`7fb756e`) | A gate on the package that exposed it, verified on B |
| 0.3.0 | 2026-08-28 | The bash PATH gap and generated-header materialization in task derivations. (`2f7b8f2`, `dc296f3`) | The generated-header example, and a package verified on B |
| 0.2.0 | 2026-09-05 | The second inherited milestone is complete, measured rather than asserted: benchmarks for generation and end-to-end compilation, a configure step cached in its own derivation, and a task's depfile declared as an output the driver reads back so a translation unit reports the headers it opened. The store-add path left the critical path. (`d0e7960`, `68905f1`, `5070d59`, `e0428a6`) | The measurement is in the commit body: on a 345 task project the driver makes 35,435 store adds, 34,371 of them answered by the stamp map for under a second between them; 25 s cold against 4 s warm over the 1,064 that genuinely upload. The figures were re-measured and corrected the same day, because the original counter recorded every upload twice (`7540642`) |
| 0.2.0 | 2026-08-30 | Depfile support, the inherited issue, in three parts and exercised end to end. (`c61fcb3`) | Recorded in the development notes with what it still owed at the time |
| 0.1.0 | 2026-09-20 | Phony target support past the inherited issue: an empty phony target is a no-op rather than a missing file, multiple CLI targets are supported, a phony guard reads its path against the right working directory, a phony marker over an existing source file stays out of the alias map, and only compiled artifacts are dropped from a compile task's phony closure. (`dcd132a`, `81b67e3`, `2f50617`, `71fdb0b`, `3b2efcc`) | Recorded in the development notes; five tests including chains and empty phonies |
| 0.1.0 | 2026-08-19 | The first inherited milestone: a CMake project builds end to end through the driver, and the Chromium-scale fixes land: store-name grammar, phony aliasing, shell no-op guards, a real `-j` cap, `passAsFile` for oversized attributes, sandbox depth mirroring, and rspfile support through a vendored n2. (`a506a5a`, `22bd862`, `a2c003e`, `52aa6ea`, `8c798e3`) | The inherited issue list marks the milestone's items complete; the corpus and the examples build |

## How a row is written

- Pin the commit that DELIVERED the work, and name the gate that verified it. A
  row whose text names a symbol must still describe that symbol.
- The text says what changed and what was measured, not that work happened.
- A row is a JUDGMENT, not a per-commit obligation. Forcing one row per commit
  would make the table meaningless, so an unrowed commit touching the source is
  a prompt to look rather than a failure.
- This fork's history begins at the upstream fork point `9a07e673db`,
  2026-05-07, and its first commit is `a506a5a`.
- The version is a MILESTONE, so it moves when the criterion in
  [the roadmap](docs/ROADMAP.md) is met, never because time passed, and never on
  its own while a re-key would land on a running build.
